//! The control protocol of the `mpv` media engine.
//!
//! mpv is started with `--input-ipc-server=PATH` and then speaks one JSON message per line over
//! that Unix socket. This module writes the commands the player needs and reads mpv's replies and
//! events, with a small JSON reader of its own (std only, bounded size and depth, never panics).
//!
//! mpv is started with [`mpv_args`], which switches off its YouTube extraction hook (`--ytdl=no`),
//! its scripts and its config files, so it plays only the local files and direct media URLs the
//! player gives it.

use crate::engine::{Backend, Event};
use crate::source::Source;
use std::io::Write;

/// Longest message accepted from mpv, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
const MAX_DEPTH: usize = 16;

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, in the order written.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The value of `key` when this is an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The text when this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The number when this is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at {}", c as char, self.i))
        }
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(format!("bad literal at {}", self.i))
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let s = self.b.get(self.i..self.i + 4).ok_or("short \\u escape")?;
        let t = std::str::from_utf8(s).map_err(|_| "bad \\u escape")?;
        let v = u32::from_str_radix(t, 16).map_err(|_| "bad \\u escape")?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err("lone surrogate".into());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("bad surrogate pair".into());
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err("lone surrogate".into());
                            } else {
                                hi
                            };
                            char::from_u32(cp).ok_or("bad code point")?
                        }
                        _ => return Err("bad escape".into()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err("control character in string".into()),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid UTF-8".to_string())
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number")?;
        let n: f64 = t.parse().map_err(|_| format!("bad number '{t}'"))?;
        if n.is_finite() {
            Ok(Json::Num(n))
        } else {
            Err("number out of range".into())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".into());
        }
        self.ws();
        match self.b.get(self.i).copied() {
            None => Err("unexpected end".into()),
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    let val = self.value(depth + 1)?;
                    v.push((k, val));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(v));
                        }
                        _ => return Err(format!("expected , or }} at {}", self.i)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(v));
                        }
                        _ => return Err(format!("expected , or ] at {}", self.i)),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(c) => Err(format!("unexpected '{}' at {}", c as char, self.i)),
        }
    }
}

/// Reads one JSON value from `text`.
///
/// # Errors
/// A message when the text is not valid JSON, is longer than [`MAX_LINE`], or is nested more
/// than 16 levels.
pub fn parse_json(text: &str) -> Result<Json, String> {
    if text.len() > MAX_LINE {
        return Err("message too long".into());
    }
    let mut r = Reader {
        b: text.as_bytes(),
        i: 0,
    };
    let v = r.value(0)?;
    r.ws();
    if r.i != r.b.len() {
        return Err(format!("unexpected text after the value at {}", r.i));
    }
    Ok(v)
}

/// A JSON string literal for `s`.
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// One argument of a command.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg<'a> {
    /// Text.
    S(&'a str),
    /// A whole number.
    I(i64),
    /// A yes/no value.
    B(bool),
}

/// A command line for mpv: `{"command":[...],"request_id":N}` and a newline.
pub fn command(request_id: u64, args: &[Arg]) -> String {
    let list: Vec<String> = args
        .iter()
        .map(|a| match a {
            Arg::S(s) => quote(s),
            Arg::I(i) => i.to_string(),
            Arg::B(b) => b.to_string(),
        })
        .collect();
    format!(
        "{{\"command\":[{}],\"request_id\":{request_id}}}\n",
        list.join(",")
    )
}

/// How a file ended in mpv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Reached the end.
    Eof,
    /// Stopped (by us, or replaced by another file).
    Stop,
    /// Could not be played.
    Error,
    /// mpv is quitting.
    Quit,
    /// Anything else.
    Other(String),
}

/// A message from mpv.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// A file finished loading and started.
    FileLoaded,
    /// A file ended.
    EndFile(EndReason),
    /// A watched property changed.
    Property {
        /// The property name.
        name: String,
        /// Its new value.
        value: Json,
    },
    /// The answer to a command.
    Reply {
        /// The id given with the command.
        request_id: u64,
        /// mpv's status text; `success` when it worked.
        status: String,
    },
    /// Some other event.
    Other(String),
}

/// Reads one line from mpv.
///
/// # Errors
/// A message when the line is not a JSON object or has no `event` or `request_id`.
pub fn parse_message(line: &str) -> Result<Msg, String> {
    let j = parse_json(line.trim())?;
    if let Some(ev) = j.get("event").and_then(Json::as_str) {
        return Ok(match ev {
            "file-loaded" => Msg::FileLoaded,
            "end-file" => Msg::EndFile(match j.get("reason").and_then(Json::as_str) {
                Some("eof") => EndReason::Eof,
                Some("stop") => EndReason::Stop,
                Some("error") => EndReason::Error,
                Some("quit") => EndReason::Quit,
                Some(o) => EndReason::Other(o.to_string()),
                None => EndReason::Other(String::new()),
            }),
            "property-change" => Msg::Property {
                name: j
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                value: j.get("data").cloned().unwrap_or(Json::Null),
            },
            other => Msg::Other(other.to_string()),
        });
    }
    if let Some(id) = j.get("request_id").and_then(Json::as_f64) {
        if id >= 0.0 && id.fract() == 0.0 && id < 1e15 {
            return Ok(Msg::Reply {
                request_id: id as u64,
                status: j
                    .get("error")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Err("not an event or a reply".into())
}

/// The player event a message means, if any.
pub fn to_event(m: &Msg) -> Option<Event> {
    match m {
        Msg::FileLoaded => Some(Event::Loaded(None)),
        Msg::EndFile(EndReason::Eof) => Some(Event::Ended),
        Msg::EndFile(EndReason::Error) => Some(Event::Error("mpv could not play this file".into())),
        // Stop and quit follow our own stop or replace commands.
        Msg::EndFile(_) => None,
        Msg::Property { name, value } => match (name.as_str(), value) {
            ("time-pos", Json::Num(n)) if *n >= 0.0 => Some(Event::Position(*n as u32)),
            ("duration", Json::Num(n)) if *n >= 0.0 => Some(Event::Duration(*n as u32)),
            ("pause", Json::Bool(true)) => Some(Event::Paused),
            ("pause", Json::Bool(false)) => Some(Event::Playing),
            _ => None,
        },
        Msg::Reply { .. } | Msg::Other(_) => None,
    }
}

/// The arguments to start mpv so the player can drive it, or why they are refused.
///
/// `vo` picks the video output (for example `gpu` or `drm`); `None` leaves mpv's own choice.
///
/// # Errors
/// A message when the socket path or output name has unusable characters.
pub fn mpv_args(socket: &str, vo: Option<&str>) -> Result<Vec<String>, String> {
    let bad = |s: &str| s.is_empty() || s.chars().any(|c| c.is_control()) || s.starts_with('-');
    if bad(socket) {
        return Err("the socket path is empty, starts with - or has control characters".into());
    }
    let mut a = vec![
        "--no-config".to_string(),
        "--idle=yes".into(),
        "--force-window=yes".into(),
        "--no-terminal".into(),
        // Never let mpv hand an address to a downloader: YouTube is played only by YouTube's player.
        "--ytdl=no".into(),
        "--load-scripts=no".into(),
        "--osc=no".into(),
        format!("--input-ipc-server={socket}"),
    ];
    if let Some(v) = vo {
        if bad(v)
            || !v
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("the video output name may only have letters, digits, - and _".into());
        }
        a.push(format!("--vo={v}"));
    }
    Ok(a)
}

/// A [`Backend`] that writes mpv commands to `W` (the socket to mpv).
pub struct MpvBackend<W: Write> {
    out: W,
    next_id: u64,
}

impl<W: Write> MpvBackend<W> {
    /// A backend writing to `out`. Call [`MpvBackend::watch`] once after connecting.
    pub fn new(out: W) -> Self {
        Self { out, next_id: 1 }
    }

    fn send(&mut self, args: &[Arg]) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        self.out
            .write_all(command(id, args).as_bytes())
            .and_then(|()| self.out.flush())
            .map_err(|e| format!("cannot talk to mpv: {e}"))
    }

    /// Asks mpv to report position, length and pause changes.
    ///
    /// # Errors
    /// A message when the socket cannot be written.
    pub fn watch(&mut self) -> Result<(), String> {
        for (n, name) in ["time-pos", "duration", "pause"].iter().enumerate() {
            self.send(&[
                Arg::S("observe_property"),
                Arg::I(n as i64 + 1),
                Arg::S(name),
            ])?;
        }
        Ok(())
    }

    /// The writer, for tests.
    pub fn into_inner(self) -> W {
        self.out
    }

    /// What has been written so far, for tests.
    pub fn written(&self) -> &W {
        &self.out
    }
}

impl<W: Write> Backend for MpvBackend<W> {
    fn load(&mut self, source: &Source) -> Result<(), String> {
        match source {
            Source::File(p) | Source::Url(p) => {
                self.send(&[Arg::S("loadfile"), Arg::S(p), Arg::S("replace")])
            }
            Source::YouTube { .. } => {
                Err("YouTube plays in YouTube's own player, not in mpv".into())
            }
        }
    }

    fn pause(&mut self, paused: bool) {
        let _ = self.send(&[Arg::S("set_property"), Arg::S("pause"), Arg::B(paused)]);
    }

    fn seek(&mut self, secs: u32) {
        let _ = self.send(&[Arg::S("seek"), Arg::I(i64::from(secs)), Arg::S("absolute")]);
    }

    fn set_volume(&mut self, percent: u8) {
        let _ = self.send(&[
            Arg::S("set_property"),
            Arg::S("volume"),
            Arg::I(i64::from(percent.min(100))),
        ]);
    }

    fn set_speed(&mut self, percent: u16) {
        let _ = self.send(&[
            Arg::S("set_property"),
            Arg::S("speed"),
            Arg::S(&format!("{}.{:02}", percent / 100, percent % 100)),
        ]);
    }

    fn stop(&mut self) {
        let _ = self.send(&[Arg::S("stop")]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::YtKind;

    fn lines(b: MpvBackend<Vec<u8>>) -> Vec<String> {
        String::from_utf8(b.into_inner())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn commands_are_one_json_line_with_a_request_id() {
        assert_eq!(
            command(7, &[Arg::S("stop")]),
            "{\"command\":[\"stop\"],\"request_id\":7}\n"
        );
        assert_eq!(
            command(1, &[Arg::S("set_property"), Arg::S("pause"), Arg::B(true)]),
            "{\"command\":[\"set_property\",\"pause\",true],\"request_id\":1}\n"
        );
        assert_eq!(
            command(2, &[Arg::S("seek"), Arg::I(-5)]),
            "{\"command\":[\"seek\",-5],\"request_id\":2}\n"
        );
    }

    #[test]
    fn a_path_with_quotes_and_newlines_cannot_break_out_of_its_string() {
        let evil = "/a\"],\"x\":[\"b\nc\\d.mp4";
        let line = command(1, &[Arg::S("loadfile"), Arg::S(evil)]);
        assert_eq!(line.matches('\n').count(), 1, "only the final newline");
        let j = parse_json(line.trim()).unwrap();
        let Json::Arr(cmd) = j.get("command").unwrap() else {
            panic!()
        };
        assert_eq!(cmd.len(), 2, "still one command with two arguments");
        assert_eq!(cmd[1].as_str(), Some(evil), "the path comes back unchanged");
        assert!(j.get("x").is_none());
    }

    #[test]
    fn json_round_trips_text_with_unicode_and_escapes() {
        for s in [
            "",
            "plain",
            "tab\there",
            "quote\"s",
            "back\\slash",
            "caf\u{e9}",
            "\u{1F600} smile",
            "ctrl\u{1}x",
            "line\nbreak",
        ] {
            let q = quote(s);
            assert_eq!(parse_json(&q).unwrap(), Json::Str(s.to_string()), "{s:?}");
        }
        assert_eq!(
            parse_json("\"\\ud83d\\ude00\"").unwrap(),
            Json::Str("\u{1F600}".into())
        );
        assert_eq!(
            parse_json("\"\\u00e9\"").unwrap(),
            Json::Str("\u{e9}".into())
        );
    }

    #[test]
    fn the_json_reader_handles_the_value_kinds() {
        let j = parse_json(
            " {\"a\": [1, 2.5, -3e2, true, false, null, \"x\"], \"b\": {\"c\": {}} , \"d\": []} ",
        )
        .unwrap();
        let Json::Arr(a) = j.get("a").unwrap() else {
            panic!()
        };
        assert_eq!(a[0], Json::Num(1.0));
        assert_eq!(a[2], Json::Num(-300.0));
        assert_eq!(a[3], Json::Bool(true));
        assert_eq!(a[5], Json::Null);
        assert_eq!(j.get("b").unwrap().get("c"), Some(&Json::Obj(vec![])));
        assert_eq!(j.get("d"), Some(&Json::Arr(vec![])));
    }

    #[test]
    fn broken_or_hostile_json_is_an_error_never_a_panic() {
        let deep = format!("{}1{}", "[".repeat(40), "]".repeat(40));
        let long = format!("\"{}\"", "a".repeat(MAX_LINE));
        for bad in [
            "",
            "{",
            "}",
            "[1,",
            "[1 2]",
            "{\"a\"}",
            "{\"a\":}",
            "{a:1}",
            "\"unterminated",
            "\"bad\\q\"",
            "\"\\u12\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"\\ud800\\u0041\"",
            "tru",
            "nul",
            "-",
            "1e",
            "1.2.3",
            "[1] x",
            "\"a\nb\"",
            "NaN",
            "1e999",
            &deep,
            &long,
            "\u{1F600}",
        ] {
            assert!(parse_json(bad).is_err(), "{:?}", &bad[..bad.len().min(30)]);
        }
        assert!(
            parse_json(&format!("{}1{}", "[".repeat(16), "]".repeat(16))).is_ok(),
            "16 levels are fine"
        );
    }

    #[test]
    fn mpv_events_become_messages_and_player_events() {
        let m = |s| parse_message(s).unwrap();
        assert_eq!(m("{\"event\":\"file-loaded\"}"), Msg::FileLoaded);
        assert_eq!(
            to_event(&m("{\"event\":\"file-loaded\"}")),
            Some(Event::Loaded(None))
        );
        assert_eq!(
            to_event(&m("{\"event\":\"end-file\",\"reason\":\"eof\"}")),
            Some(Event::Ended)
        );
        assert!(matches!(
            to_event(&m("{\"event\":\"end-file\",\"reason\":\"error\"}")),
            Some(Event::Error(_))
        ));
        assert_eq!(
            to_event(&m("{\"event\":\"end-file\",\"reason\":\"stop\"}")),
            None,
            "our own stop or replace"
        );
        assert_eq!(
            to_event(&m("{\"event\":\"end-file\",\"reason\":\"quit\"}")),
            None
        );
        assert_eq!(
            to_event(&m(
                "{\"event\":\"property-change\",\"id\":1,\"name\":\"time-pos\",\"data\":12.9}"
            )),
            Some(Event::Position(12))
        );
        assert_eq!(
            to_event(&m(
                "{\"event\":\"property-change\",\"id\":2,\"name\":\"duration\",\"data\":90.0}"
            )),
            Some(Event::Duration(90))
        );
        assert_eq!(
            to_event(&m(
                "{\"event\":\"property-change\",\"id\":3,\"name\":\"pause\",\"data\":true}"
            )),
            Some(Event::Paused)
        );
        assert_eq!(
            to_event(&m(
                "{\"event\":\"property-change\",\"id\":3,\"name\":\"pause\",\"data\":false}"
            )),
            Some(Event::Playing)
        );
        assert_eq!(
            to_event(&m(
                "{\"event\":\"property-change\",\"id\":1,\"name\":\"time-pos\"}"
            )),
            None,
            "no data yet"
        );
        assert_eq!(
            to_event(&m(
                "{\"event\":\"property-change\",\"name\":\"time-pos\",\"data\":-1}"
            )),
            None
        );
        assert_eq!(to_event(&m("{\"event\":\"idle\"}")), None);
        assert_eq!(
            m("{\"error\":\"success\",\"request_id\":4}"),
            Msg::Reply {
                request_id: 4,
                status: "success".into()
            }
        );
        assert_eq!(
            to_event(&m("{\"error\":\"success\",\"data\":null,\"request_id\":4}")),
            None
        );
    }

    #[test]
    fn messages_that_are_neither_events_nor_replies_are_errors() {
        for bad in [
            "{}",
            "[]",
            "1",
            "{\"data\":1}",
            "{\"request_id\":-1}",
            "{\"request_id\":1.5}",
            "{\"request_id\":1e30}",
            "not json",
        ] {
            assert!(parse_message(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_backend_writes_the_right_commands_and_refuses_youtube() {
        let mut b = MpvBackend::new(Vec::new());
        b.watch().unwrap();
        b.load(&Source::File("/m/a.mp4".into())).unwrap();
        b.load(&Source::Url("https://x.org/v.webm".into())).unwrap();
        assert!(b
            .load(&Source::YouTube {
                id: "dQw4w9WgXcQ".into(),
                kind: YtKind::Short
            })
            .is_err());
        b.pause(true);
        b.seek(30);
        b.set_volume(250);
        b.set_speed(125);
        b.set_speed(50);
        b.stop();
        let l = lines(b);
        assert_eq!(
            l.len(),
            3 + 2 + 6,
            "watch x3, load x2, then pause seek volume speed speed stop"
        );
        assert!(
            l[0].contains("observe_property")
                && l[0].contains("time-pos")
                && l[1].contains("duration")
                && l[2].contains("\"pause\"")
        );
        assert!(l[3].contains("\"loadfile\",\"/m/a.mp4\",\"replace\""));
        assert!(l[4].contains("https://x.org/v.webm"));
        assert!(l[5].contains("\"pause\",true"));
        assert!(l[6].contains("\"seek\",30,\"absolute\""));
        assert!(
            l[7].contains("\"volume\",100"),
            "volume is capped at 100: {}",
            l[7]
        );
        assert!(l[8].contains("\"speed\",\"1.25\"") && l[9].contains("\"speed\",\"0.50\""));
        assert!(l[10].contains("\"stop\""));
        let ids: Vec<u64> = l
            .iter()
            .map(|s| {
                parse_json(s)
                    .unwrap()
                    .get("request_id")
                    .unwrap()
                    .as_f64()
                    .unwrap() as u64
            })
            .collect();
        assert_eq!(
            ids,
            (1..=11).collect::<Vec<u64>>(),
            "every command has its own id"
        );
    }

    #[test]
    fn a_dead_socket_is_reported_on_load_and_does_not_panic_elsewhere() {
        struct Dead;
        impl Write for Dead {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "closed",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut b = MpvBackend::new(Dead);
        assert!(b
            .load(&Source::File("/a.mp4".into()))
            .unwrap_err()
            .contains("cannot talk to mpv"));
        b.pause(true);
        b.seek(1);
        b.stop();
        assert!(b.watch().is_err());
    }

    #[test]
    fn mpv_is_started_without_its_youtube_hook_scripts_or_config() {
        let a = mpv_args("/run/mouli/mpv.sock", Some("drm")).unwrap();
        for must in [
            "--no-config",
            "--ytdl=no",
            "--load-scripts=no",
            "--idle=yes",
            "--input-ipc-server=/run/mouli/mpv.sock",
            "--vo=drm",
        ] {
            assert!(a.iter().any(|x| x == must), "missing {must}: {a:?}");
        }
        assert!(!mpv_args("/s", None)
            .unwrap()
            .iter()
            .any(|x| x.starts_with("--vo")));
        assert!(mpv_args("", None).is_err());
        assert!(mpv_args("--script=evil.lua", None).is_err());
        assert!(mpv_args("/s\n", None).is_err());
        assert!(mpv_args("/s", Some("gpu --script=x")).is_err());
        assert!(mpv_args("/s", Some("-x")).is_err());
    }

    #[test]
    fn it_drives_the_player_end_to_end_with_mpv_style_messages() {
        use crate::engine::{Command, Player, State};
        use crate::queue::Item;
        let mut p = Player::new(MpvBackend::new(Vec::new()));
        p.queue_mut()
            .push(Item::new(Source::File("/m/a.mp4".into())));
        p.queue_mut()
            .push(Item::new(Source::File("/m/b.mp4".into())));
        p.command(Command::Play);
        for line in [
            "{\"event\":\"file-loaded\"}",
            "{\"event\":\"property-change\",\"id\":2,\"name\":\"duration\",\"data\":60.0}",
            "{\"event\":\"end-file\",\"reason\":\"eof\"}",
        ] {
            if let Some(ev) = to_event(&parse_message(line).unwrap()) {
                p.on_event(ev);
            }
        }
        assert_eq!(*p.state(), State::Loading, "the second file was asked for");
        let sent = String::from_utf8(p.backend().written().clone()).unwrap();
        assert!(sent.contains("/m/a.mp4") && sent.contains("/m/b.mp4"));
    }
}
