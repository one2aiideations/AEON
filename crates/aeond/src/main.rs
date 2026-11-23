//! aeond entry point. See the library docs.
#![forbid(unsafe_code)]

use aeon_hw::{Hw, Plan};
use aeond::args::{Args, DEFAULT_MODEL_DIR};
use aeond::models::{describe, discover, Found};
use aeond::ui::{chat_loop, pick_model, Backend, Choice, End, Turn};
use aeond::{cmdline_has, find_model, http, system};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::Duration;

const SYSTEM_PROMPT: &str = "You are AEON, a helpful assistant running entirely on this computer with no internet connection. Answer clearly and concisely.";
const REPLY_TOKENS: u32 = 512;

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
        Err(format!("the model did not load (see {})", log_path(pid1)))
    }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn finish(pid1: bool, code: u8) -> ExitCode {
    if pid1 {
        say("AEON: powering off");
        say(&system::power_off());
        loop {
            sleep(Duration::from_secs(3600));
        }
    }
    ExitCode::from(code)
}

struct ServerBackend(u16);

impl Backend for ServerBackend {
    fn reply(&mut self, history: &[Turn]) -> Result<String, String> {
        let turns: Vec<(&str, &str)> = history
            .iter()
            .map(|t| (t.role, t.content.as_str()))
            .collect();
        http::chat_messages(self.0, &turns, REPLY_TOKENS)
    }
}

fn selftest(a: &Args, plan: &Plan, pid1: bool) -> ExitCode {
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

fn interactive(a: &Args, plan: &Plan, pid1: bool) -> ExitCode {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    loop {
        let models: Vec<Found> = match &a.model {
            Some(m) => describe(m).into_iter().collect(),
            None => discover(Path::new(DEFAULT_MODEL_DIR), pid1),
        };
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
            Choice::Model(i) => {
                let m = &models[i];
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
                    &mut ServerBackend(a.port),
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
    if args.selftest {
        selftest(&args, &plan, pid1)
    } else {
        interactive(&args, &plan, pid1)
    }
}
