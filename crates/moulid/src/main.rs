//! moulid entry point. See the library docs.
#![forbid(unsafe_code)]

use mouli_utility::hw::{Hw, Plan};
use moulid::args::{Args, DEFAULT_MODEL_DIR};
use moulid::bluetooth;
use moulid::clocksync;
use moulid::gui;
use moulid::hosted::{self, HostedBackend};
use moulid::hwprobe;
use moulid::models::{describe, discover, Found};
use moulid::net;
use moulid::persist::{self, Store};
use moulid::ui::{chat_loop, pick_model_checked, Backend, Choice, End, Turn};
use moulid::verify;
use moulid::wifi;
use moulid::wifi_saved;
use moulid::{catalog, download_ui, fetch};
use moulid::{cmdline_has, find_model, http, system};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

const SYSTEM_PROMPT: &str = "You are MOULI, a helpful assistant running entirely on this computer with no internet connection. Answer clearly and concisely.";
const REPLY_TOKENS: u32 = 512;
/// Conversation budget for hosted models (their context is large; this keeps cost sensible).
const HOSTED_CONTEXT_TOKENS: usize = 6_000;

/// Mount point of the data partition, when there is one (unmounted before power-off).
static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn say(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// Set the clock from the network and say what happened (HTTPS needs a correct clock).
fn sync_clock_say() {
    match clocksync::sync() {
        Ok(m) => say(&format!("MOULI clock: {m}")),
        Err(e) => say(&format!("MOULI clock: not set from the network ({e})")),
    }
}

fn log_path(pid1: bool) -> &'static str {
    if pid1 {
        "/var/log/mouli-inference.log"
    } else {
        "/tmp/mouli-inference.log"
    }
}

fn start_server(a: &Args, model: &Path, plan: &Plan, pid1: bool) -> std::io::Result<Child> {
    let log = std::fs::File::create(log_path(pid1))?;
    Command::new(&a.server)
        .args(["-m"])
        .arg(model)
        .args(["--host", "127.0.0.1", "--port", &a.port.to_string()])
        .args([
            "-c",
            &plan.ctx.to_string(),
            "-t",
            &plan.threads.to_string(),
            "--no-webui",
        ])
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
}

fn wait_healthy(port: u16, child: &mut Child, seconds: u32) -> bool {
    for _ in 0..seconds * 2 {
        if http::healthy(port) {
            return true;
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            return false;
        }
        sleep(Duration::from_millis(500));
    }
    false
}

/// Start the inference server for `model` and wait until it answers.
fn launch(a: &Args, model: &Path, plan: &Plan, pid1: bool) -> Result<Child, String> {
    let mut child = start_server(a, model, plan, pid1)
        .map_err(|e| format!("cannot start {}: {e}", a.server.display()))?;
    if wait_healthy(a.port, &mut child, 300) {
        Ok(child)
    } else {
        let _ = child.kill();
        let _ = child.wait();
        let log = std::fs::read_to_string(log_path(pid1)).unwrap_or_default();
        say(&format!(
            "---- last lines of {} ----\n{}\n----",
            log_path(pid1),
            moulid::tail_lines(&log, 20)
        ));
        Err(format!("the model did not load (see {})", log_path(pid1)))
    }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Unmounts the data disk and restarts the machine (when running as PID 1).
fn restart(pid1: bool) -> ExitCode {
    if pid1 {
        if let Some(root) = DATA_ROOT.get() {
            persist::unmount(root);
        }
        say("MOULI: restarting");
        say(&system::reboot());
        loop {
            sleep(Duration::from_secs(3600));
        }
    }
    ExitCode::from(0)
}

fn finish(pid1: bool, code: u8) -> ExitCode {
    if pid1 {
        if let Some(root) = DATA_ROOT.get() {
            persist::unmount(root);
        }
        say("MOULI: powering off");
        say(&system::power_off());
        loop {
            sleep(Duration::from_secs(3600));
        }
    }
    ExitCode::from(code)
}

/// Talks to the local server and, when there is a data partition, logs each turn.
struct ServerBackend {
    port: u16,
    log: Option<(Store, String)>,
}

impl ServerBackend {
    fn record(&self, role: &str, text: &str) {
        if let Some((store, session)) = &self.log {
            let _ = store.log_turn(session, role, text);
        }
    }
}

impl Backend for ServerBackend {
    fn reply(&mut self, history: &[Turn]) -> Result<String, String> {
        let turns: Vec<(&str, &str)> = history
            .iter()
            .map(|t| (t.role, t.content.as_str()))
            .collect();
        if let Some(last) = history.last() {
            self.record(last.role, &last.content);
        }
        let answer = http::chat_messages(self.port, &turns, REPLY_TOKENS)?;
        self.record("assistant", &answer);
        Ok(answer)
    }

    fn reply_streaming(
        &mut self,
        history: &[Turn],
        on_piece: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String> {
        let turns: Vec<(&str, &str)> = history
            .iter()
            .map(|t| (t.role, t.content.as_str()))
            .collect();
        if let Some(last) = history.last() {
            self.record(last.role, &last.content);
        }
        let answer = http::chat_messages_stream(self.port, &turns, REPLY_TOKENS, on_piece)?;
        self.record("assistant", &answer);
        Ok(answer)
    }
}

fn selftest(a: &Args, plan: &Plan, pid1: bool, store: Option<&Store>) -> ExitCode {
    let model = a
        .model
        .clone()
        .or_else(|| find_model(Path::new(DEFAULT_MODEL_DIR)));
    let Some(model) = model else {
        say(&format!("MOULI: no model found in {DEFAULT_MODEL_DIR}"));
        say("MOULI selftest: FAIL (no model)");
        return finish(pid1, 1);
    };
    say(&format!(
        "MOULI: starting inference server with {}",
        model.display()
    ));
    let mut child = match launch(a, &model, plan, pid1) {
        Ok(c) => c,
        Err(e) => {
            say(&format!("MOULI: {e}"));
            say("MOULI selftest: FAIL (server)");
            return finish(pid1, 1);
        }
    };
    say("MOULI ready");
    let code = match http::chat(a.port, "hello MOULI", 8) {
        Ok(r) => {
            say(&format!("MOULI chat reply: {r}"));
            if let Some(s) = store {
                let session = s.new_session();
                let saved = s.set("selftest", "ok").is_ok()
                    && s.log_turn(&session, "user", "hello MOULI").is_ok()
                    && s.log_turn(&session, "assistant", &r).is_ok();
                say(if saved {
                    "MOULI data: saved settings and chat log"
                } else {
                    "MOULI data: saving failed"
                });
            }
            say("MOULI selftest: PASS");
            0
        }
        Err(e) => {
            say(&format!("MOULI selftest: FAIL (chat: {e})"));
            1
        }
    };
    stop(&mut child);
    finish(pid1, code)
}

fn interactive(a: &Args, plan: &Plan, pid1: bool, store: Option<&Store>) -> ExitCode {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    loop {
        let mut models: Vec<Found> = match &a.model {
            Some(m) => describe(m).into_iter().collect(),
            None => discover(Path::new(DEFAULT_MODEL_DIR), pid1),
        };
        if let (Some(s), None) = (store, &a.model) {
            models.extend(moulid::models::scan_dir(
                &s.models_dir(),
                moulid::models::SCAN_DEPTH,
            ));
            let mut seen = std::collections::BTreeSet::new();
            models.retain(|f| seen.insert(f.path.clone()));
            // The model used last time goes first, so Enter-1 resumes where you left off.
            if let Some(last) = s.settings().get("last_model") {
                models.sort_by_key(|f| {
                    f.path
                        .file_name()
                        .is_none_or(|n| n.to_string_lossy() != *last)
                });
            }
        }
        let choice = if a.model.is_some() && models.len() == 1 {
            Choice::Model(0)
        } else {
            let (entries, _) = catalog::load(Path::new(catalog::CATALOG_FILE));
            // A file the catalogue lists is checked against its size and SHA-256 before loading.
            let mut verify_file = |m: &Found| -> Option<String> {
                let name = m.path.file_name()?.to_string_lossy().into_owned();
                if !verify::known(&name, &entries) {
                    return None;
                }
                say(&format!("MOULI: checking {name} against the catalogue..."));
                match verify::check(&m.path, &entries) {
                    Ok(c) => verify::warning(&name, &c),
                    Err(e) => Some(format!("{name} could not be read to check it: {e}")),
                }
            };
            match pick_model_checked(
                &models,
                plan.model_budget_mb,
                plan.ctx,
                &mut input,
                &mut out,
                &mut verify_file,
            ) {
                Ok(c) => c,
                Err(_) => {
                    sleep(Duration::from_secs(1));
                    continue;
                }
            }
        };
        match choice {
            Choice::Quit => return finish(pid1, 0),
            Choice::Rescan => continue,
            Choice::Info => {
                let text = hwprobe::report(Path::new("/proc"), Path::new("/sys"));
                let _ = writeln!(out, "\n{text}");
                if let Some(s) = store {
                    let saved = std::fs::write(s.root().join("hardware-report.txt"), &text);
                    let _ = writeln!(
                        out,
                        "{}",
                        if saved.is_ok() {
                            "  (also saved on the data disk as hardware-report.txt)"
                        } else {
                            "  (could not save a copy)"
                        }
                    );
                }
                continue;
            }
            Choice::Download => {
                let Some(st) = store else {
                    say("MOULI: downloading needs a MOULI-DATA disk to keep the model on (see the installation guide).");
                    continue;
                };
                let (entries, warning) = catalog::load(Path::new(catalog::CATALOG_FILE));
                if let Some(w) = warning {
                    say(&format!("MOULI: {w}"));
                }
                match fetch::HttpFetcher::system() {
                    Ok(fetcher) => {
                        let _ = download_ui::flow(
                            &mut input,
                            &mut out,
                            &fetcher,
                            &fetch::Policy::production(),
                            &entries,
                            &st.root().join("models"),
                            plan.model_budget_mb,
                        );
                    }
                    Err(e) => say(&format!("MOULI: {e}")),
                }
                continue;
            }
            Choice::Bluetooth => {
                if let Some(s) = store {
                    if let Err(e) =
                        bluetooth::persist_keys(s.root(), Path::new(bluetooth::STATE_DIR))
                    {
                        say(&format!("MOULI bluetooth: {e}"));
                    }
                } else {
                    say(
                        "MOULI bluetooth: no MOULI-DATA disk, so pairing is forgotten at power-off",
                    );
                }
                let _ = bluetooth::pair_flow(
                    &mut input,
                    &mut out,
                    &mut wifi::System,
                    Path::new("/sys/class/rfkill"),
                    Path::new(bluetooth::SYS_BLUETOOTH),
                );
                continue;
            }
            Choice::Wifi => {
                let joined = wifi::connect_flow(
                    &mut input,
                    &mut out,
                    &mut wifi::System,
                    Path::new(net::SYS_NET),
                    store.map(persist::Store::root),
                );
                if matches!(joined, Ok(Some(_))) {
                    sync_clock_say();
                }
                continue;
            }
            Choice::Hosted => {
                let Ok(Some((provider, _saved))) = hosted::setup(&mut input, &mut out, store)
                else {
                    continue;
                };
                let mut backend = match HostedBackend::new(provider, store) {
                    Ok(b) => b,
                    Err(e) => {
                        say(&format!("MOULI: {e}"));
                        continue;
                    }
                };
                say("MOULI ready (hosted model)");
                let end = chat_loop(
                    &mut backend,
                    &mut input,
                    &mut out,
                    hosted::SYSTEM_PROMPT,
                    HOSTED_CONTEXT_TOKENS,
                );
                match end {
                    Ok(End::SwitchModel) if a.model.is_none() => {}
                    Ok(End::Reboot) => return restart(pid1),
                    Ok(End::Quit | End::SwitchModel) => return finish(pid1, 0),
                    Ok(End::Eof) | Err(_) => {
                        if !pid1 {
                            return finish(pid1, 0);
                        }
                        sleep(Duration::from_secs(1));
                    }
                }
            }
            Choice::Model(i) => {
                let m = &models[i];
                if let (Some(s), Some(name)) = (store, m.path.file_name()) {
                    let _ = s.set("last_model", &name.to_string_lossy());
                }
                say(&format!(
                    "MOULI: loading {} ({} MB). This can take a minute...",
                    m.name,
                    m.size_mb()
                ));
                if let Some(why) = m.why_not(plan.model_budget_mb, plan.ctx) {
                    say(&format!("MOULI: warning: {} {why}", m.name));
                }
                // The picker already checked the file; a model named on the command line skips
                // it, so check it here and warn (it is not refused: it was asked for by name).
                if a.model.is_some() {
                    let (entries, _) = catalog::load(Path::new(catalog::CATALOG_FILE));
                    if let Some(name) = m.path.file_name().map(|n| n.to_string_lossy().into_owned())
                    {
                        if verify::known(&name, &entries) {
                            say(&format!("MOULI: checking {name} against the catalogue..."));
                            let found = verify::check(&m.path, &entries);
                            if let Some(w) = found.ok().and_then(|c| verify::warning(&name, &c)) {
                                say(&format!("MOULI: warning: {w}"));
                            }
                        }
                    }
                }
                let mut child = match launch(a, &m.path, plan, pid1) {
                    Ok(c) => c,
                    Err(e) => {
                        say(&format!("MOULI: {e}"));
                        if a.model.is_some() {
                            return finish(pid1, 1);
                        }
                        continue;
                    }
                };
                say("MOULI ready");
                // Keep about 80% of the server's context for the conversation; the rest is for
                // the reply and for the estimate being rough.
                let max_tokens = usize::try_from(plan.ctx).unwrap_or(2048).saturating_mul(4) / 5;
                let mut backend = ServerBackend {
                    port: a.port,
                    log: store.map(|s| (s.clone(), s.new_session())),
                };
                let text_only = cmdline_has(&system::kernel_cmdline(), "mouli.text=1");
                let session = if text_only {
                    None
                } else {
                    match gui::start(store.map(persist::Store::root)) {
                        Ok(s) => {
                            say(&s.inputs().line());
                            Some(s)
                        }
                        Err(e) => {
                            say(&format!("MOULI: no desktop ({e}); using the text chat"));
                            None
                        }
                    }
                };
                let end = match session {
                    Some(s) => {
                        let info = gui::ModelsInfo {
                            rows: models
                                .iter()
                                .map(|f| mouli_screen::screens::ModelRow {
                                    name: f.name.clone(),
                                    size_mb: f.size_mb(),
                                    arch: f.arch.clone(),
                                    needs_mb: f.needs_mb(plan.ctx),
                                    fits: f.fits(plan.model_budget_mb, plan.ctx),
                                })
                                .collect(),
                            budget_mb: plan.model_budget_mb,
                            ctx: plan.ctx,
                            loaded: m.name.clone(),
                        };
                        gui::run(s, backend, SYSTEM_PROMPT, max_tokens, &info)
                    }
                    None => chat_loop(
                        &mut backend,
                        &mut input,
                        &mut out,
                        SYSTEM_PROMPT,
                        max_tokens,
                    ),
                };
                stop(&mut child);
                match end {
                    Ok(End::SwitchModel) if a.model.is_none() => {}
                    Ok(End::Reboot) => return restart(pid1),
                    Ok(End::Quit | End::SwitchModel) => return finish(pid1, 0),
                    Ok(End::Eof) | Err(_) => {
                        if !pid1 {
                            return finish(pid1, 0);
                        }
                        sleep(Duration::from_secs(1));
                    }
                }
            }
        }
    }
}

/// Open the data partition (or, in development mode, the directory in `MOULI_DATA_DIR`) and say
/// what will be saved.
fn open_store(pid1: bool) -> Option<Store> {
    let root = if pid1 {
        persist::mount_data()
    } else {
        std::env::var_os("MOULI_DATA_DIR").map(PathBuf::from)
    };
    let store = root.as_deref().and_then(|r| Store::open(r).ok());
    match &store {
        Some(s) => {
            let _ = DATA_ROOT.set(s.root().to_path_buf());
            say(&format!(
                "MOULI: saving settings and chat history to {} (label {})",
                s.root().display(),
                persist::DATA_LABEL
            ));
        }
        None => say(&format!(
            "MOULI: no disk labelled {} found - nothing will be saved after power-off",
            persist::DATA_LABEL
        )),
    }
    store
}

fn main() -> ExitCode {
    // Before any thread exists, so SIGTERM and SIGHUP can be handled instead of killing us.
    #[cfg(target_os = "linux")]
    let _ = moulid::shutdown::block_stop_signals();
    let mut args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("moulid: {e}");
            return ExitCode::from(2);
        }
    };
    let pid1 = system::is_pid1();
    if pid1 {
        for w in system::early_setup() {
            say(&format!("moulid: {w}"));
        }
    }
    if cmdline_has(&system::kernel_cmdline(), "mouli.selftest=1") {
        args.selftest = true;
    }
    if moulid::banner::use_color(&system::kernel_cmdline()) {
        say(&moulid::banner::palette_sequence());
        say(&moulid::banner::banner_color(moulid::VERSION));
    } else {
        say(&moulid::banner::banner(moulid::VERSION));
    }
    say(&format!(
        "MOULI {} - AI-native OS{}",
        moulid::VERSION,
        if pid1 { "" } else { " (development mode)" }
    ));
    let hw = Hw::probe();
    let plan = hw.plan();
    say(&hw.format(&plan));
    if cfg!(target_arch = "x86_64") && !hw.avx2 {
        say("MOULI warning: this CPU has no AVX2; the inference engine is built for AVX2 and may crash.");
    }
    let mut wired_up = false;
    if pid1 {
        if net::clock_from_rtc() {
            say("MOULI clock: set from the hardware clock");
        }
        let wired = net::bring_up_wired(3);
        wired_up = matches!(wired, net::Link::Up { .. });
        say(&wired.summary());
        if wired_up {
            match clocksync::sync() {
                Ok(m) => say(&format!("MOULI clock: {m}")),
                Err(e) => say(&format!("MOULI clock: not set from the network ({e})")),
            }
        }
        let _ = wifi::unblock_radios(Path::new("/sys/class/rfkill"));
        say(&wifi::summary(&wifi::wireless_interfaces(Path::new(
            net::SYS_NET,
        ))));
        if cmdline_has(&system::kernel_cmdline(), "mouli.wifitest=1") {
            return match wifi::selftest(&mut wifi::System, Path::new(net::SYS_NET)) {
                Ok(()) => {
                    say("MOULI wifi test: PASS (scanned, refused a wrong password, joined with the right one)");
                    finish(pid1, 0)
                }
                Err(e) => {
                    say(&format!("MOULI wifi test: FAIL ({e})"));
                    finish(pid1, 1)
                }
            };
        }
    }
    let store = open_store(pid1);
    if pid1 {
        let sys = Path::new(bluetooth::SYS_BLUETOOTH);
        let adapters = bluetooth::adapters(sys);
        say(&bluetooth::summary(&adapters));
        // A mouse paired in an earlier session reconnects by itself once the service is running.
        if let (Some(s), false) = (&store, adapters.is_empty()) {
            if bluetooth::has_saved_pairings(s.root())
                && bluetooth::persist_keys(s.root(), Path::new(bluetooth::STATE_DIR)).is_ok()
            {
                match bluetooth::start_services(
                    &mut wifi::System,
                    Path::new("/sys/class/rfkill"),
                    sys,
                ) {
                    Ok(()) => say("MOULI bluetooth: ready (paired devices reconnect when used)"),
                    Err(e) => say(&format!("MOULI bluetooth: {e}")),
                }
            }
        }
    }
    if pid1 && !wired_up {
        if let Some(s) = &store {
            if let Some((ssid, result)) =
                wifi_saved::auto_connect(&mut wifi::System, Path::new(net::SYS_NET), s.root())
            {
                match result {
                    Ok(link) => say(&format!(
                        "MOULI Wi-Fi: joined saved network {ssid}. {}",
                        link.summary()
                    )),
                    Err(e) => say(&format!("MOULI Wi-Fi: saved network {ssid}: {e}")),
                }
            }
        }
    }
    if args.selftest {
        selftest(&args, &plan, pid1, store.as_ref())
    } else {
        interactive(&args, &plan, pid1, store.as_ref())
    }
}
