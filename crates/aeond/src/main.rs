//! aeond entry point. See the library docs.
#![forbid(unsafe_code)]

use aeon_hw::Hw;
use aeond::args::{Args, DEFAULT_MODEL_DIR};
use aeond::{cmdline_has, find_model, http, system};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::Duration;

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

fn start_server(
    a: &Args,
    model: &Path,
    threads: u32,
    ctx: u32,
    pid1: bool,
) -> std::io::Result<Child> {
    let log = std::fs::File::create(log_path(pid1))?;
    Command::new(&a.server)
        .args(["-m"])
        .arg(model)
        .args(["--host", "127.0.0.1", "--port", &a.port.to_string()])
        .args([
            "-c",
            &ctx.to_string(),
            "-t",
            &threads.to_string(),
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

fn finish(pid1: bool, child: Option<&mut Child>, code: u8) -> ExitCode {
    if let Some(c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    if pid1 {
        say("AEON: powering off");
        say(&system::power_off());
        loop {
            sleep(Duration::from_secs(3600));
        }
    }
    ExitCode::from(code)
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
        "AEON {} - AI-native OS init{}",
        aeond::VERSION,
        if pid1 {
            " (PID 1)"
        } else {
            " (development mode)"
        }
    ));

    let hw = Hw::probe();
    let plan = hw.plan();
    say(&hw.format(&plan));

    let model: Option<PathBuf> = args
        .model
        .clone()
        .or_else(|| find_model(Path::new(DEFAULT_MODEL_DIR)));
    let Some(model) = model else {
        say(&format!("AEON: no model found in {DEFAULT_MODEL_DIR}"));
        if args.selftest {
            say("AEON selftest: FAIL (no model)");
        }
        return finish(pid1, None, 1);
    };

    let stdin = std::io::stdin();
    loop {
        say(&format!(
            "AEON: starting inference server with {}",
            model.display()
        ));
        let mut child = match start_server(&args, &model, plan.threads, plan.ctx, pid1) {
            Ok(c) => c,
            Err(e) => {
                say(&format!(
                    "AEON: cannot start {}: {e}",
                    args.server.display()
                ));
                if args.selftest {
                    say("AEON selftest: FAIL (server)");
                    return finish(pid1, None, 1);
                }
                sleep(Duration::from_secs(5));
                continue;
            }
        };
        if !wait_healthy(args.port, &mut child, 300) {
            say(&format!(
                "AEON: inference server failed to become ready (see {})",
                log_path(pid1)
            ));
            if args.selftest {
                say("AEON selftest: FAIL (server)");
                return finish(pid1, Some(&mut child), 1);
            }
            let _ = child.kill();
            let _ = child.wait();
            sleep(Duration::from_secs(5));
            continue;
        }
        say("AEON ready");
        if args.selftest {
            return match http::chat(args.port, "hello AEON", 8) {
                Ok(r) => {
                    say(&format!("AEON chat reply: {r}"));
                    say("AEON selftest: PASS");
                    finish(pid1, Some(&mut child), 0)
                }
                Err(e) => {
                    say(&format!("AEON selftest: FAIL (chat: {e})"));
                    finish(pid1, Some(&mut child), 1)
                }
            };
        }
        say("Type a message and press Enter (Ctrl-D to stop).");
        let mut line = String::new();
        loop {
            print!("you> ");
            let _ = std::io::stdout().flush();
            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) if !pid1 => return finish(pid1, Some(&mut child), 0),
                Ok(0) | Err(_) => {
                    sleep(Duration::from_secs(1));
                    continue;
                }
                Ok(_) => {}
            }
            if matches!(child.try_wait(), Ok(Some(_))) {
                say("aeond: inference server exited; restarting");
                break;
            }
            let prompt = line.trim();
            if prompt.is_empty() {
                continue;
            }
            match http::chat(args.port, prompt, 128) {
                Ok(r) => say(&format!("aeon> {r}")),
                Err(e) => say(&format!("aeon> (no reply: {e})")),
            }
        }
    }
}
