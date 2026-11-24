//! Console interface: a model picker and a chat screen, written against `BufRead` and `Write` so
//! they work on the physical console, a serial line, or in tests.

use crate::models::Found;
use std::io::{self, BufRead, Write};

/// What the person chose in the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Model number (zero based index into the list).
    Model(usize),
    /// Look again for models (a USB stick may have been inserted).
    Rescan,
    /// Use a hosted model over the network.
    Hosted,
    /// Connect to a Wi-Fi network.
    Wifi,
    /// Show the hardware report.
    Info,
    /// Power off.
    Quit,
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// `system`, `user` or `assistant`.
    pub role: &'static str,
    /// Text.
    pub content: String,
}

/// Something that answers a conversation.
pub trait Backend {
    /// Reply to the whole history.
    ///
    /// # Errors
    /// A message to show the person when the model cannot answer.
    fn reply(&mut self, history: &[Turn]) -> Result<String, String>;
}

/// Why the chat screen ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Go back to the picker.
    SwitchModel,
    /// Power off.
    Quit,
    /// Input closed.
    Eof,
}

/// Text of the picker.
#[must_use]
pub fn render_menu(models: &[Found], budget_mb: u64, ctx: u32) -> String {
    let mut s = String::from("\n  Choose a model\n  --------------\n");
    if models.is_empty() {
        s.push_str(
            "  No models found.\n  Copy a .gguf model file onto a USB stick or disk, plug it in,\n  then press r and Enter to look again.\n",
        );
    }
    for (i, m) in models.iter().enumerate() {
        let fit = if m.fits(budget_mb, ctx) {
            "fits in memory"
        } else {
            "TOO BIG for this machine's free memory"
        };
        let needs = if m.gguf.is_some() {
            format!(", needs ~{} MB", m.needs_mb(ctx))
        } else {
            String::new()
        };
        s.push_str(&format!(
            "  {:>2}) {}  [{}{}{} MB{}, {}]\n      {}\n",
            i + 1,
            m.name,
            m.arch
                .as_deref()
                .map_or(String::new(), |a| format!("{a}, ")),
            m.quant().map_or(String::new(), |q| format!("{q}, ")),
            m.size_mb(),
            needs,
            fit,
            m.path.display()
        ));
    }
    s.push_str("\n  Type a number and press Enter.  r = look again   w = Wi-Fi   h = hosted model (online)   i = hardware info   q = power off\n");
    s
}

/// Parse one line typed in the picker. Numbers are one-based.
#[must_use]
pub fn parse_choice(line: &str, n: usize) -> Option<Choice> {
    let t = line.trim().to_ascii_lowercase();
    match t.as_str() {
        "r" => Some(Choice::Rescan),
        "h" => Some(Choice::Hosted),
        "w" => Some(Choice::Wifi),
        "i" => Some(Choice::Info),
        "q" => Some(Choice::Quit),
        _ => t
            .parse::<usize>()
            .ok()
            .filter(|k| (1..=n).contains(k))
            .map(|k| Choice::Model(k - 1)),
    }
}

/// Show the picker and read a valid choice. End of input counts as quit.
///
/// # Errors
/// I/O errors on the console.
pub fn pick_model<R: BufRead, W: Write>(
    models: &[Found],
    budget_mb: u64,
    ctx: u32,
    input: &mut R,
    out: &mut W,
) -> io::Result<Choice> {
    write!(out, "{}", render_menu(models, budget_mb, ctx))?;
    loop {
        write!(out, "  > ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Choice::Quit);
        }
        if let Some(c) = parse_choice(&line, models.len()) {
            return Ok(c);
        }
        writeln!(
            out,
            "  Please type a number from 1 to {}, r, w, h, i or q.",
            models.len()
        )?;
    }
}

/// Drop the oldest turns until the history fits `max_tokens` (estimated, see
/// [`crate::context`]), always keeping the system prompt and the newest turn; a newest turn that
/// is too long on its own is cut. Returns what was done: `(turns dropped, newest turn cut)`.
pub fn trim_history(history: &mut Vec<Turn>, max_tokens: usize) -> (usize, bool) {
    use crate::context::{History, Role};
    let system = history
        .first()
        .filter(|t| t.role == "system")
        .map(|t| t.content.clone());
    let skip = usize::from(system.is_some());
    let mut ctx = History::new();
    for t in history.iter().skip(skip) {
        let role = if t.role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        };
        ctx.push(role, t.content.clone());
    }
    let fitted = ctx.fit(max_tokens, system.as_deref().unwrap_or(""));
    if fitted.dropped == 0 && !fitted.truncated {
        return (0, false);
    }
    let mut rebuilt = Vec::with_capacity(fitted.turns.len() + skip);
    if let Some(content) = system {
        rebuilt.push(Turn {
            role: "system",
            content,
        });
    }
    for t in fitted.turns {
        rebuilt.push(Turn {
            role: if t.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            },
            content: t.text,
        });
    }
    *history = rebuilt;
    (fitted.dropped, fitted.truncated)
}

const HELP: &str = "  /models  choose another model\n  /clear   start a new conversation\n  /help    this list\n  /quit    power off\n";

/// The chat screen. Reads lines until the person quits, switches model or input ends.
///
/// # Errors
/// I/O errors on the console.
pub fn chat_loop<R: BufRead, W: Write, B: Backend>(
    backend: &mut B,
    input: &mut R,
    out: &mut W,
    system_prompt: &str,
    max_tokens: usize,
) -> io::Result<End> {
    let fresh = || {
        vec![Turn {
            role: "system",
            content: system_prompt.into(),
        }]
    };
    let mut history = fresh();
    writeln!(
        out,
        "\n  Ready. Type a message and press Enter.  /help for commands.\n"
    )?;
    loop {
        write!(out, "you> ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(End::Eof);
        }
        let text = line.trim();
        match text {
            "" => continue,
            "/quit" | "/exit" => return Ok(End::Quit),
            "/models" => return Ok(End::SwitchModel),
            "/help" => {
                write!(out, "{HELP}")?;
                continue;
            }
            "/clear" => {
                history = fresh();
                writeln!(out, "  (new conversation)")?;
                continue;
            }
            _ if text.starts_with('/') => {
                writeln!(out, "  Unknown command. /help lists them.")?;
                continue;
            }
            _ => {}
        }
        history.push(Turn {
            role: "user",
            content: text.into(),
        });
        let (dropped, cut) = trim_history(&mut history, max_tokens);
        if dropped > 0 {
            writeln!(
                out,
                "  (the conversation is long: {dropped} oldest message(s) were forgotten to fit the model's memory)"
            )?;
        }
        if cut {
            writeln!(out, "  (your message was too long and was shortened)")?;
        }
        writeln!(out, "mouli> (thinking...)")?;
        out.flush()?;
        match backend.reply(&history) {
            Ok(r) => {
                writeln!(out, "mouli> {}\n", r.trim())?;
                history.push(Turn {
                    role: "assistant",
                    content: r,
                });
            }
            Err(e) => {
                history.pop();
                writeln!(out, "mouli> (no reply: {e})\n")?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model(name: &str, mb: u64) -> Found {
        Found {
            path: PathBuf::from(format!("/mnt/sda1/{name}.gguf")),
            size: mb << 20,
            name: name.into(),
            arch: Some("llama".into()),
            context: None,
            gguf: None,
        }
    }

    struct Echo(Vec<usize>);
    impl Backend for Echo {
        fn reply(&mut self, h: &[Turn]) -> Result<String, String> {
            self.0.push(h.len());
            let last = &h[h.len() - 1].content;
            if last == "fail" {
                Err("server down".into())
            } else {
                Ok(format!("echo: {last}"))
            }
        }
    }

    fn run(script: &str) -> (End, String, Vec<usize>) {
        let (mut b, mut out) = (Echo(vec![]), Vec::new());
        let end = chat_loop(&mut b, &mut script.as_bytes(), &mut out, "be nice", 10_000).unwrap();
        (end, String::from_utf8(out).unwrap(), b.0)
    }

    #[test]
    fn choices() {
        assert_eq!(parse_choice(" 2 \n", 3), Some(Choice::Model(1)));
        assert_eq!(parse_choice("R", 0), Some(Choice::Rescan));
        assert_eq!(parse_choice("q", 0), Some(Choice::Quit));
        assert_eq!(parse_choice(" I \n", 0), Some(Choice::Info));
        for bad in ["0", "4", "-1", "x", ""] {
            assert_eq!(parse_choice(bad, 3), None, "{bad}");
        }
    }

    #[test]
    fn menu_text() {
        let m = render_menu(&[model("small", 100), model("huge", 90_000)], 4000, 2048);
        assert!(m.contains(" 1) small") && m.contains("fits in memory") && m.contains("TOO BIG"));
        assert!(m.contains("/mnt/sda1/small.gguf"));
        assert!(render_menu(&[], 0, 2048).contains("No models found"));
    }

    #[test]
    fn picker_retries_until_valid() {
        let ms = [model("a", 1), model("b", 1)];
        let mut out = Vec::new();
        let c = pick_model(&ms, 100, 2048, &mut "9\nzzz\n2\n".as_bytes(), &mut out).unwrap();
        assert_eq!(c, Choice::Model(1));
        assert_eq!(
            String::from_utf8(out)
                .unwrap()
                .matches("Please type")
                .count(),
            2
        );
        assert_eq!(
            pick_model(&ms, 100, 2048, &mut "".as_bytes(), &mut Vec::new()).unwrap(),
            Choice::Quit,
            "eof quits"
        );
        assert_eq!(
            pick_model(&[], 100, 2048, &mut "r\n".as_bytes(), &mut Vec::new()).unwrap(),
            Choice::Rescan
        );
    }

    #[test]
    fn chat_keeps_history_and_handles_errors() {
        let (end, out, lens) = run("hello\nfail\nagain\n/quit\n");
        assert_eq!(end, End::Quit);
        assert!(
            out.contains("mouli> echo: hello")
                && out.contains("no reply: server down")
                && out.contains("mouli> echo: again")
        );
        assert_eq!(lens, [2, 4, 4], "failed turn is removed from history");
    }

    #[test]
    fn commands() {
        let (end, out, lens) = run("hi\n/clear\n/help\n/nope\nhi\n/models\n");
        assert_eq!(end, End::SwitchModel);
        assert!(
            out.contains("new conversation")
                && out.contains("/quit    power off")
                && out.contains("Unknown command")
        );
        assert_eq!(lens, [2, 2], "clear drops history");
        assert_eq!(run("").0, End::Eof);
        assert_eq!(run("/exit\n").0, End::Quit);
        assert_eq!(
            run("\n\n  \n/quit\n").2,
            Vec::<usize>::new(),
            "blank lines are ignored"
        );
    }

    #[test]
    fn trimming_keeps_system_and_newest() {
        let t = |r, c: &str| Turn {
            role: r,
            content: c.into(),
        };
        let mut h = vec![
            t("system", "sys"),
            t("user", &"a".repeat(700)),
            t("assistant", &"b".repeat(700)),
            t("user", &"c".repeat(700)),
        ];
        // Everything fits: nothing changes.
        assert_eq!(trim_history(&mut h, 10_000), (0, false));
        assert_eq!(h.len(), 4);
        // Room for the system prompt and about one long turn.
        let (dropped, cut) = trim_history(&mut h, 260);
        assert_eq!((dropped, cut), (2, false));
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, "system");
        assert_eq!(h[1].content, "c".repeat(700));
        // Far too small: the newest turn is cut but kept.
        let (_, cut) = trim_history(&mut h, 20);
        assert!(cut);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, "system");
        assert!(h[1].content.ends_with("[cut]"));
    }

    #[test]
    fn trimming_never_starts_with_an_assistant_turn() {
        let t = |r, c: &str| Turn {
            role: r,
            content: c.into(),
        };
        let mut h = vec![
            t("system", "sys"),
            t("user", &"a".repeat(400)),
            t("assistant", &"b".repeat(400)),
            t("user", "short"),
        ];
        trim_history(&mut h, 60);
        assert_eq!(h[1].role, "user");
    }

    #[test]
    fn long_chats_tell_the_person_what_was_forgotten() {
        let long = "word ".repeat(200);
        let script = format!("{long}\n{long}\n{long}\n/quit\n");
        let (mut b, mut out) = (Echo(vec![]), Vec::new());
        chat_loop(&mut b, &mut script.as_bytes(), &mut out, "be nice", 600).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("oldest message(s) were forgotten"), "{out}");
    }
}
