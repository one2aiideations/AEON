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
    /// Pair a Bluetooth mouse or trackpad.
    Bluetooth,
    /// Download a model.
    Download,
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

    /// Like [`Backend::reply`], but calls `on_piece` with each piece of text as it arrives;
    /// `on_piece` returns false to stop, and the text so far is then returned as the reply.
    /// A backend that cannot stream answers whole: nothing is shown until it is done and a
    /// stop only takes effect after it.
    ///
    /// # Errors
    /// As [`Backend::reply`].
    fn reply_streaming(
        &mut self,
        history: &[Turn],
        on_piece: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String> {
        let _ = on_piece;
        self.reply(history)
    }
}

/// Why the chat screen ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Go back to the picker.
    SwitchModel,
    /// Power off.
    Quit,
    /// Restart.
    Reboot,
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
        if let Some(why) = m.why_not(budget_mb, ctx) {
            s.push_str(&format!("      It {why}\n"));
        }
    }
    s.push_str("\n  Type a number and press Enter.  r = look again   w = Wi-Fi   b = Bluetooth mouse   d = download a model   h = hosted model (online)   i = hardware info   q = power off\n");
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
        "b" => Some(Choice::Bluetooth),
        "d" => Some(Choice::Download),
        "i" => Some(Choice::Info),
        "q" => Some(Choice::Quit),
        _ => t
            .parse::<usize>()
            .ok()
            .filter(|k| (1..=n).contains(k))
            .map(|k| Choice::Model(k - 1)),
    }
}

/// Show the picker and read a valid choice, with no check of the file itself (see
/// [`pick_model_checked`]).
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
    pick_model_checked(models, budget_mb, ctx, input, out, &mut |_| None)
}

/// Show the picker and read a valid choice. End of input counts as quit.
///
/// Before a model is returned, `verify` is asked about it: if it returns a problem (the file is
/// not the one the catalogue lists, say) the person sees it and chooses between loading anyway (`y`)
/// and going back.
///
/// A model that does not fit the memory budget is not loaded straight away: the reason is shown
/// (see [`Found::why_not`]) and the person chooses the model that does fit (`f`), loading it anyway
/// (`y`), or going back.
///
/// # Errors
/// I/O errors on the console.
pub fn pick_model_checked<R: BufRead, W: Write>(
    models: &[Found],
    budget_mb: u64,
    ctx: u32,
    input: &mut R,
    out: &mut W,
    verify: &mut dyn FnMut(&Found) -> Option<String>,
) -> io::Result<Choice> {
    write!(out, "{}", render_menu(models, budget_mb, ctx))?;
    loop {
        write!(out, "  > ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Choice::Quit);
        }
        match parse_choice(&line, models.len()) {
            Some(Choice::Model(i)) => match confirm_fit(models, i, budget_mb, ctx, input, out)? {
                Fit::Load(j) => {
                    let Some(problem) = verify(&models[j]) else {
                        return Ok(Choice::Model(j));
                    };
                    writeln!(out, "\n  {problem}")?;
                    write!(
                        out,
                        "  y = load it anyway   anything else = choose again\n  > "
                    )?;
                    out.flush()?;
                    let mut answer = String::new();
                    if input.read_line(&mut answer)? == 0 {
                        return Ok(Choice::Quit);
                    }
                    if answer.trim().eq_ignore_ascii_case("y") {
                        return Ok(Choice::Model(j));
                    }
                }
                Fit::Again => {}
                Fit::Eof => return Ok(Choice::Quit),
            },
            Some(c) => return Ok(c),
            None => writeln!(
                out,
                "  Please type a number from 1 to {}, r, w, h, i or q.",
                models.len()
            )?,
        }
    }
}

/// What to do after a model was chosen.
enum Fit {
    Load(usize),
    Again,
    Eof,
}

/// A model that fits loads at once. One that does not gets its reason and a choice.
fn confirm_fit<R: BufRead, W: Write>(
    models: &[Found],
    i: usize,
    budget_mb: u64,
    ctx: u32,
    input: &mut R,
    out: &mut W,
) -> io::Result<Fit> {
    let Some(why) = models[i].why_not(budget_mb, ctx) else {
        return Ok(Fit::Load(i));
    };
    writeln!(out, "\n  {} {why}", models[i].name)?;
    let other = crate::models::fallback(models, i, budget_mb, ctx);
    match other {
        Some(j) => writeln!(
            out,
            "  f = use {} instead (fits, about {} MB)   y = load it anyway (it may fail or be very slow)   anything else = choose again",
            models[j].name,
            models[j].needs_mb(ctx)
        )?,
        None => writeln!(
            out,
            "  Nothing else on this machine fits.   y = load it anyway (it may fail or be very slow)   anything else = choose again"
        )?,
    }
    write!(out, "  > ")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(Fit::Eof);
    }
    Ok(match (line.trim().to_ascii_lowercase().as_str(), other) {
        ("f", Some(j)) => Fit::Load(j),
        ("y", _) => Fit::Load(i),
        _ => Fit::Again,
    })
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

    /// A model whose header says its layers and width, so the attention cache can be worked out.
    fn model_with_cache(name: &str, mb: u64, layers: u64, width: u64) -> Found {
        let mut m = model(name, mb);
        m.gguf = Some(crate::gguf_info::GgufInfo {
            version: 3,
            tensor_count: 1,
            architecture: Some("llama".into()),
            name: None,
            file_type: None,
            context_length: None,
            embedding_length: Some(width),
            block_count: Some(layers),
            header_bytes_read: 64,
        });
        m
    }

    #[test]
    fn a_model_that_does_not_fit_says_where_the_memory_goes_and_what_would_fit() {
        // 3000 MB file + 1024 MB cache (32 layers x 4096 wide x 2048 tokens x 4 bytes) + 256 MB.
        let m = model_with_cache("m", 3000, 32, 4096);
        assert!(m.why_not(5000, 2048).is_none());
        let why = m.why_not(3500, 2048).unwrap();
        assert!(why.contains("needs about 4280 MB"), "{why}");
        assert!(
            why.contains("3000 MB file + 1024 MB attention cache + 256 MB working memory"),
            "{why}"
        );
        assert!(why.contains("only 3500 MB is free"), "{why}");
        assert!(why.contains("up to 448 tokens"), "{why}");
        // When the file alone is too big no context would help.
        let huge = model_with_cache("h", 5000, 32, 4096);
        let why = huge.why_not(3500, 512).unwrap();
        assert!(why.contains("file alone does not fit"), "{why}");
        // Without header information it still explains itself.
        let plain = model("p", 9000);
        assert!(plain.why_not(3500, 2048).unwrap().contains("smaller model"));
    }

    #[test]
    fn the_fallback_is_the_largest_model_that_fits_and_is_no_bigger() {
        use crate::models::fallback;
        let ms = [
            model("big", 5000),
            model("mid", 3000),
            model("small", 1000),
            model("huge", 9000),
        ];
        assert_eq!(fallback(&ms, 0, 3500, 512), Some(1), "big -> mid");
        assert_eq!(
            fallback(&ms, 3, 3500, 512),
            Some(1),
            "huge -> mid, not the one that is too big"
        );
        // Nothing smaller fits, so the largest that does fit is offered.
        let ms = [model("tiny", 400), model("mid", 3000), model("big", 5000)];
        assert_eq!(fallback(&ms, 0, 100, 512), None, "nothing fits at all");
        assert_eq!(fallback(&ms, 0, 3500, 512), Some(1));
        assert_eq!(fallback(&[model("only", 9000)], 0, 100, 512), None);
        assert_eq!(
            fallback(&ms, 9, 3500, 512),
            Some(1),
            "an index out of range is not a crash"
        );
    }

    #[test]
    fn choosing_a_model_that_does_not_fit_shows_why_and_asks() {
        let ms = [model("small", 1000), model("big", 9000)];
        let run = |input: &str| {
            let mut out = Vec::new();
            let c = pick_model(&ms, 3500, 2048, &mut input.as_bytes(), &mut out).unwrap();
            (c, String::from_utf8(out).unwrap())
        };
        // The menu itself already gives the reason.
        let (_, text) = run("q\n");
        assert!(text.contains("It needs at least 9000 MB"), "{text}");
        // f takes the one that fits; y loads it anyway; anything else asks again.
        let (c, text) = run("2\nf\n");
        assert_eq!(c, Choice::Model(0));
        assert!(text.contains("f = use small instead"), "{text}");
        assert_eq!(run("2\ny\n").0, Choice::Model(1));
        assert_eq!(run("2\nno\n1\n").0, Choice::Model(0), "back to choosing");
        assert_eq!(run("2\n").0, Choice::Quit, "end of input while asked");
        // A model that fits loads with no questions.
        assert_eq!(run("1\n").0, Choice::Model(0));
        // With nothing else that fits, f is not offered and does nothing.
        let ms2 = [model("big", 9000)];
        let mut out = Vec::new();
        let c = pick_model(&ms2, 3500, 2048, &mut "1\nf\nq\n".as_bytes(), &mut out).unwrap();
        assert_eq!(c, Choice::Quit);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("Nothing else on this machine fits"));
    }

    #[test]
    fn a_file_that_fails_its_check_is_loaded_only_if_the_person_says_so() {
        let ms = [model("a", 100), model("b", 100)];
        let run = |input: &str| {
            let mut out = Vec::new();
            let mut asked = Vec::new();
            let c =
                pick_model_checked(&ms, 4000, 2048, &mut input.as_bytes(), &mut out, &mut |m| {
                    asked.push(m.name.clone());
                    (m.name == "b").then(|| "b is not the catalogue's copy.".to_string())
                })
                .unwrap();
            (c, asked, String::from_utf8(out).unwrap())
        };
        // A model that passes loads with no question.
        let (c, asked, text) = run("1\n");
        assert_eq!((c, asked), (Choice::Model(0), vec!["a".to_string()]));
        assert!(!text.contains("load it anyway"));
        // One that fails is shown the problem; only y loads it.
        let (c, _, text) = run("2\ny\n");
        assert_eq!(c, Choice::Model(1));
        assert!(
            text.contains("b is not the catalogue's copy.") && text.contains("y = load it anyway"),
            "{text}"
        );
        assert_eq!(
            run("2\nn\n1\n").0,
            Choice::Model(0),
            "anything else goes back"
        );
        assert_eq!(run("2\n").0, Choice::Quit, "end of input");
        // The plain picker never checks.
        let mut out = Vec::new();
        assert_eq!(
            pick_model(&ms, 4000, 2048, &mut "2\n".as_bytes(), &mut out).unwrap(),
            Choice::Model(1)
        );
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
