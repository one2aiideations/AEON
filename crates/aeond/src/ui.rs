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
pub fn render_menu(models: &[Found], budget_mb: u64) -> String {
    let mut s = String::from("\n  Choose a model\n  --------------\n");
    if models.is_empty() {
        s.push_str(
            "  No models found.\n  Copy a .gguf model file onto a USB stick or disk, plug it in,\n  then press r and Enter to look again.\n",
        );
    }
    for (i, m) in models.iter().enumerate() {
        let fit = if m.fits(budget_mb) {
            "fits in memory"
        } else {
            "TOO BIG for this machine's free memory"
        };
        s.push_str(&format!(
            "  {:>2}) {}  [{}{} MB, {}]\n      {}\n",
            i + 1,
            m.name,
            m.arch
                .as_deref()
                .map_or(String::new(), |a| format!("{a}, ")),
            m.size_mb(),
            fit,
            m.path.display()
        ));
    }
    s.push_str("\n  Type a number and press Enter.  r = look again   q = power off\n");
    s
}

/// Parse one line typed in the picker. Numbers are one-based.
#[must_use]
pub fn parse_choice(line: &str, n: usize) -> Option<Choice> {
    let t = line.trim().to_ascii_lowercase();
    match t.as_str() {
        "r" => Some(Choice::Rescan),
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
    input: &mut R,
    out: &mut W,
) -> io::Result<Choice> {
    write!(out, "{}", render_menu(models, budget_mb))?;
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
            "  Please type a number from 1 to {}, r or q.",
            models.len()
        )?;
    }
}

/// Drop the oldest turns until the history fits `max_chars`, always keeping the system prompt
/// and the newest turn.
pub fn trim_history(history: &mut Vec<Turn>, max_chars: usize) {
    let total = |h: &[Turn]| h.iter().map(|t| t.content.len()).sum::<usize>();
    while total(history) > max_chars {
        let first = usize::from(history.first().is_some_and(|t| t.role == "system"));
        if history.len() <= first + 1 {
            break;
        }
        history.remove(first);
    }
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
    max_chars: usize,
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
        trim_history(&mut history, max_chars);
        writeln!(out, "aeon> (thinking...)")?;
        out.flush()?;
        match backend.reply(&history) {
            Ok(r) => {
                writeln!(out, "aeon> {}\n", r.trim())?;
                history.push(Turn {
                    role: "assistant",
                    content: r,
                });
            }
            Err(e) => {
                history.pop();
                writeln!(out, "aeon> (no reply: {e})\n")?;
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
        for bad in ["0", "4", "-1", "x", ""] {
            assert_eq!(parse_choice(bad, 3), None, "{bad}");
        }
    }

    #[test]
    fn menu_text() {
        let m = render_menu(&[model("small", 100), model("huge", 90_000)], 4000);
        assert!(m.contains(" 1) small") && m.contains("fits in memory") && m.contains("TOO BIG"));
        assert!(m.contains("/mnt/sda1/small.gguf"));
        assert!(render_menu(&[], 0).contains("No models found"));
    }

    #[test]
    fn picker_retries_until_valid() {
        let ms = [model("a", 1), model("b", 1)];
        let mut out = Vec::new();
        let c = pick_model(&ms, 100, &mut "9\nzzz\n2\n".as_bytes(), &mut out).unwrap();
        assert_eq!(c, Choice::Model(1));
        assert_eq!(
            String::from_utf8(out)
                .unwrap()
                .matches("Please type")
                .count(),
            2
        );
        assert_eq!(
            pick_model(&ms, 100, &mut "".as_bytes(), &mut Vec::new()).unwrap(),
            Choice::Quit,
            "eof quits"
        );
        assert_eq!(
            pick_model(&[], 100, &mut "r\n".as_bytes(), &mut Vec::new()).unwrap(),
            Choice::Rescan
        );
    }

    #[test]
    fn chat_keeps_history_and_handles_errors() {
        let (end, out, lens) = run("hello\nfail\nagain\n/quit\n");
        assert_eq!(end, End::Quit);
        assert!(
            out.contains("aeon> echo: hello")
                && out.contains("no reply: server down")
                && out.contains("aeon> echo: again")
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
            t("user", &"a".repeat(50)),
            t("assistant", &"b".repeat(50)),
            t("user", &"c".repeat(50)),
        ];
        trim_history(&mut h, 120);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].role, "system");
        trim_history(&mut h, 10);
        assert_eq!(
            h.len(),
            2,
            "never drops the system prompt or the newest turn"
        );
        assert_eq!(h[1].content, "c".repeat(50));
    }
}
