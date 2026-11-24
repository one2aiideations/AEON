//! aeond entry point. See the library docs.
#![forbid(unsafe_code)]

use aeon_hw::{Hw, Plan};
use aeond::args::{Args, DEFAULT_MODEL_DIR};
use aeond::hosted::{self, HostedBackend};
use aeond::models::{describe, discover, Found};
use aeond::net;
use aeond::persist::{self, Store};
use aeond::ui::{chat_loop, pick_model, Backend, Choice, End, Turn};
use aeond::wifi;
use aeond::wifi_saved;
use aeond::{cmdline_has, find_model, http, system};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

const SYSTEM_PROMPT: &str = "You are AEON, a helpful assistant running entirely on this computer with no internet connection. Answer clearly and concisely.";
const REPLY_TOKENS: u32 = 512;

/// Mount point of the data partition, when there is one (unmounted before power-off).
static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn say(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn log_path(pid1: bool) -> &'static str {
    if pid1 {
        "/var/log/aeon-inference.log"
    } else {
        "/tmp/aeon-inference.log"
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
            aeond::tail_lines(&log, 20)
        ));
        Err(format!("the model did not load (see {})", log_path(pid1)))
    }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn finish(pid1: bool, code: u8) -> ExitCode {
    if pid1 {
        if let Some(root) = DATA_ROOT.get() {
            persist::unmount(root);
        }
        say("AEON: powering off");
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
}

fn selftest(a: &Args, plan: &Plan, pid1: bool, store: Option<&Store>) -> ExitCode {
    let model = a
        .model
        .clone()
        .or_else(|| find_model(Path::new(DEFAULT_MODEL_DIR)));
    let Some(model) = model else {
        say(&format!("AEON: no model found in {DEFAULT_MODEL_DIR}"));
        say("AEON selftest: FAIL (no model)");
        return finish(pid1, 1);
    };
    say(&format!(
        "AEON: starting inference server with {}",
        model.display()
    ));
    let mut child = match launch(a, &model, plan, pid1) {
        Ok(c) => c,
        Err(e) => {
            say(&format!("AEON: {e}"));
            say("AEON selftest: FAIL (server)");
            return finish(pid1, 1);
        }
    };
    say("AEON ready");
    let code = match http::chat(a.port, "hello AEON", 8) {
        Ok(r) => {
            say(&format!("AEON chat reply: {r}"));
            if let Some(s) = store {
                let session = s.new_session();
                let saved = s.set("selftest", "ok").is_ok()
                    && s.log_turn(&session, "user", "hello AEON").is_ok()
                    && s.log_turn(&session, "assistant", &r).is_ok();
                say(if saved {
                    "AEON data: saved settings and chat log"
                } else {
                    "AEON data: saving failed"
                });
            }
            say("AEON selftest: PASS");
            0
        }
        Err(e) => {
            say(&format!("AEON selftest: FAIL (chat: {e})"));
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
            models.extend(aeond::models::scan_dir(
                &s.models_dir(),
                aeond::models::SCAN_DEPTH,
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
            match pick_model(&models, plan.model_budget_mb, &mut input, &mut out) {
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
            Choice::Wifi => {
                let _ = wifi::connect_flow(
                    &mut input,
                    &mut out,
                    &mut wifi::System,
                    Path::new(net::SYS_NET),
                    store.map(persist::Store::root),
                );
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
                        say(&format!("AEON: {e}"));
                        continue;
                    }
                };
                say("AEON ready (hosted model)");
                let end = chat_loop(
                    &mut backend,
                    &mut input,
                    &mut out,
                    hosted::SYSTEM_PROMPT,
                    12_000,
                );
                match end {
                    Ok(End::SwitchModel) if a.model.is_none() => {}
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
                    "AEON: loading {} ({} MB). This can take a minute...",
                    m.name,
                    m.size_mb()
                ));
                let mut child = match launch(a, &m.path, plan, pid1) {
                    Ok(c) => c,
                    Err(e) => {
                        say(&format!("AEON: {e}"));
                        if a.model.is_some() {
                            return finish(pid1, 1);
                        }
                        continue;
                    }
                };
                say("AEON ready");
                let max_chars = usize::try_from(plan.ctx).unwrap_or(2048).saturating_mul(3);
                let end = chat_loop(
                    &mut ServerBackend {
                        port: a.port,
                        log: store.map(|s| (s.clone(), s.new_session())),
                    },
                    &mut input,
                    &mut out,
                    SYSTEM_PROMPT,
                    max_chars,
                );
                stop(&mut child);
                match end {
                    Ok(End::SwitchModel) if a.model.is_none() => {}
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

/// Open the data partition (or, in development mode, the directory in `AEON_DATA_DIR`) and say
/// what will be saved.
fn open_store(pid1: bool) -> Option<Store> {
    let root = if pid1 {
        persist::mount_data()
    } else {
        std::env::var_os("AEON_DATA_DIR").map(PathBuf::from)
    };
    let store = root.as_deref().and_then(|r| Store::open(r).ok());
    match &store {
        Some(s) => {
            let _ = DATA_ROOT.set(s.root().to_path_buf());
            say(&format!(
                "AEON: saving settings and chat history to {} (label {})",
                s.root().display(),
                persist::DATA_LABEL
            ));
        }
        None => say(&format!(
            "AEON: no disk labelled {} found - nothing will be saved after power-off",
            persist::DATA_LABEL
        )),
    }
    store
}

fn main() -> ExitCode {
    let mut args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("aeond: {e}");
            return ExitCode::from(2);
        }
    };
    let pid1 = system::is_pid1();
    if pid1 {
        for w in system::early_setup() {
            say(&format!("aeond: {w}"));
        }
    }
    if cmdline_has(&system::kernel_cmdline(), "aeon.selftest=1") {
        args.selftest = true;
    }
    say(&format!(
        "AEON {} - AI-native OS{}",
        aeond::VERSION,
        if pid1 { "" } else { " (development mode)" }
    ));
    let hw = Hw::probe();
    let plan = hw.plan();
    say(&hw.format(&plan));
    if cfg!(target_arch = "x86_64") && !hw.avx2 {
        say("AEON warning: this CPU has no AVX2; the inference engine is built for AVX2 and may crash.");
    }
    let mut wired_up = false;
    if pid1 {
        if net::clock_from_rtc() {
            say("AEON clock: set from the hardware clock");
        }
        let wired = net::bring_up_wired(3);
        wired_up = matches!(wired, net::Link::Up { .. });
        say(&wired.summary());
        let _ = wifi::unblock_radios(Path::new("/sys/class/rfkill"));
        say(&wifi::summary(&wifi::wireless_interfaces(Path::new(
            net::SYS_NET,
        ))));
        if cmdline_has(&system::kernel_cmdline(), "aeon.wifitest=1") {
            return match wifi::selftest(&mut wifi::System, Path::new(net::SYS_NET)) {
                Ok(()) => {
                    say("AEON wifi test: PASS (scanned, refused a wrong password, joined with the right one)");
                    finish(pid1, 0)
                }
                Err(e) => {
                    say(&format!("AEON wifi test: FAIL ({e})"));
                    finish(pid1, 1)
                }
            };
        }
    }
    let store = open_store(pid1);
    if pid1 && !wired_up {
        if let Some(s) = &store {
            if let Some((ssid, result)) =
                wifi_saved::auto_connect(&mut wifi::System, Path::new(net::SYS_NET), s.root())
            {
                match result {
                    Ok(link) => say(&format!(
                        "AEON Wi-Fi: joined saved network {ssid}. {}",
                        link.summary()
                    )),
                    Err(e) => say(&format!("AEON Wi-Fi: saved network {ssid}: {e}")),
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
