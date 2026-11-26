//! Minimal HTTP/1.0 client and flat-JSON helpers for the local inference server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Escape `s` as the inside of a JSON string.
#[must_use]
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// String value of top-level `"key"` in a flat JSON object, unescaped.
///
/// Handles `\" \\ \/ \b \f \n \r \t` and `\uXXXX` (with surrogate pairs).
#[must_use]
pub fn json_get_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut rest = &json[json.find(&needle)? + needle.len()..];
    rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hi = hex4(&mut chars)?;
                    let cp = if (0xD800..0xDC00).contains(&hi) {
                        if chars.next()? != '\\' || chars.next()? != 'u' {
                            return None;
                        }
                        let lo = hex4(&mut chars)?;
                        if !(0xDC00..0xE000).contains(&lo) {
                            return None;
                        }
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    } else {
                        hi
                    };
                    out.push(char::from_u32(cp)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
}

fn hex4(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let mut v = 0u32;
    for _ in 0..4 {
        v = v * 16 + chars.next()?.to_digit(16)?;
    }
    Some(v)
}

/// Split `HTTP/1.x NNN ...\r\n...\r\n\r\nbody` into status and body.
#[must_use]
pub fn parse_response(resp: &str) -> Option<(u16, &str)> {
    let rest = resp.strip_prefix("HTTP/1.")?;
    let mut parts = rest.get(2..)?.splitn(2, ' ');
    let status: u16 = parts.next()?.trim().parse().ok()?;
    let body = match resp.find("\r\n\r\n") {
        Some(i) => &resp[i + 4..],
        None => &resp[resp.find("\n\n")? + 2..],
    };
    Some((status, body))
}

/// Blocking request to `127.0.0.1:port`; returns status and body.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> std::io::Result<(u16, String)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let body = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut resp = String::new();
    s.read_to_string(&mut resp)?;
    let (status, b) = parse_response(&resp)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad HTTP response"))?;
    Ok((status, b.to_owned()))
}

/// Send `prompt` to `/completion` and return the generated text.
pub fn chat(port: u16, prompt: &str, n_predict: u32) -> Result<String, String> {
    let body = format!(
        "{{\"prompt\":\"{}\",\"n_predict\":{n_predict},\"temperature\":0,\"cache_prompt\":true}}",
        json_escape(prompt)
    );
    let (status, resp) = request(
        port,
        "POST",
        "/completion",
        Some(&body),
        Duration::from_secs(120),
    )
    .map_err(|e| format!("request failed: {e}"))?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    json_get_string(&resp, "content").ok_or_else(|| "no content in reply".into())
}

/// The `,"stop":[...]` part of a request, or nothing when there are no stop strings.
fn stop_field(stop: &[String]) -> String {
    if stop.is_empty() {
        return String::new();
    }
    let items: Vec<String> = stop
        .iter()
        .map(|s| format!("\"{}\"", json_escape(s)))
        .collect();
    format!(",\"stop\":[{}]", items.join(","))
}

/// Send a whole conversation to `/v1/chat/completions` (the server applies the model's chat
/// template) and return the assistant's text.
pub fn chat_messages(
    port: u16,
    turns: &[(&str, &str)],
    max_tokens: u32,
    stop: &[String],
) -> Result<String, String> {
    let msgs: Vec<String> = turns
        .iter()
        .map(|(r, c)| format!("{{\"role\":\"{r}\",\"content\":\"{}\"}}", json_escape(c)))
        .collect();
    let body = format!(
        "{{\"messages\":[{}],\"max_tokens\":{max_tokens},\"temperature\":0.7,\"stream\":false{}}}",
        msgs.join(","),
        stop_field(stop)
    );
    let (status, resp) = request(
        port,
        "POST",
        "/v1/chat/completions",
        Some(&body),
        Duration::from_secs(600),
    )
    .map_err(|e| format!("request failed: {e}"))?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    json_get_string(&resp, "content").ok_or_else(|| "no content in reply".into())
}

/// Reads a server-sent event stream of chat chunks until `[DONE]`, the end of the stream, or
/// `on_piece` returning false. Returns the text received so far and whether the server finished
/// the answer by itself.
fn read_chat_events<R: BufRead>(
    mut r: R,
    on_piece: &mut dyn FnMut(&str) -> bool,
) -> Result<(String, bool), String> {
    let mut text = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)
            .map_err(|e| format!("read failed: {e}"))?
            == 0
        {
            return Ok((text, false));
        }
        let Some(data) = line.trim_end().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            return Ok((text, true));
        }
        if let Some(piece) = json_get_string(data, "content") {
            if !piece.is_empty() {
                text.push_str(&piece);
                if !on_piece(&piece) {
                    return Ok((text, false));
                }
            }
        }
    }
}

/// Like [`chat_messages`], but streams: `on_piece` gets each piece of text as the server
/// produces it and returns false to stop (the connection is closed and the text so far is the
/// result).
///
/// # Errors
/// A message when the server cannot be reached, answers with an error, or the stream breaks
/// before the server finished without the caller having asked to stop.
pub fn chat_messages_stream(
    port: u16,
    turns: &[(&str, &str)],
    max_tokens: u32,
    stop: &[String],
    on_piece: &mut dyn FnMut(&str) -> bool,
) -> Result<String, String> {
    let msgs: Vec<String> = turns
        .iter()
        .map(|(r, c)| format!("{{\"role\":\"{r}\",\"content\":\"{}\"}}", json_escape(c)))
        .collect();
    let body = format!(
        "{{\"messages\":[{}],\"max_tokens\":{max_tokens},\"temperature\":0.7,\"stream\":true{}}}",
        msgs.join(","),
        stop_field(stop)
    );
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = Duration::from_secs(600);
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("request failed: {e}"))?;
    s.set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    write!(
        s,
        "POST /v1/chat/completions HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| format!("request failed: {e}"))?;
    let mut r = BufReader::new(s);
    let mut status_line = String::new();
    r.read_line(&mut status_line)
        .map_err(|e| format!("read failed: {e}"))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("bad HTTP response")?;
    let mut header = String::new();
    loop {
        header.clear();
        if r.read_line(&mut header)
            .map_err(|e| format!("read failed: {e}"))?
            == 0
            || header.trim().is_empty()
        {
            break;
        }
    }
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    let mut stopped = false;
    let (text, finished) = read_chat_events(r, &mut |p| {
        let keep = on_piece(p);
        stopped = !keep;
        keep
    })?;
    if finished || stopped {
        Ok(text)
    } else {
        Err("the model's answer was cut off".into())
    }
}

/// True when `/health` answers 200.
#[must_use]
pub fn healthy(port: u16) -> bool {
    matches!(
        request(port, "GET", "/health", None, Duration::from_secs(2)),
        Ok((200, _))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_piece_by_piece_until_done() {
        let stream =
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
            : keep-alive\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"lo \\\"w\\u00e9\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
            data: [DONE]\n\n";
        let mut pieces = Vec::new();
        let (text, done) = read_chat_events(stream.as_bytes(), &mut |p| {
            pieces.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(pieces, vec!["Hel", "lo \"wé"]);
        assert_eq!(text, "Hello \"wé");
        assert!(done);
    }

    #[test]
    fn a_stop_request_ends_reading_and_a_cut_stream_is_not_finished() {
        let stream =
            "data: {\"delta\":{\"content\":\"a\"}}\ndata: {\"delta\":{\"content\":\"b\"}}\n";
        let mut n = 0;
        let (text, done) = read_chat_events(stream.as_bytes(), &mut |_| {
            n += 1;
            false
        })
        .unwrap();
        assert_eq!((text.as_str(), done, n), ("a", false, 1));
        let (text, done) = read_chat_events(stream.as_bytes(), &mut |_| true).unwrap();
        assert_eq!((text.as_str(), done), ("ab", false));
    }

    #[test]
    fn streaming_from_a_local_server_end_to_end() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf).unwrap();
            let body = "data: {\"delta\":{\"content\":\"Hi\"}}\n\ndata: {\"delta\":{\"content\":\" there\"}}\n\ndata: [DONE]\n\n";
            write!(
                c,
                "HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}"
            )
            .unwrap();
        });
        let mut got = Vec::new();
        let text = chat_messages_stream(port, &[("user", "hi")], 8, &[], &mut |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        t.join().unwrap();
        assert_eq!(text, "Hi there");
        assert_eq!(got, vec!["Hi", " there"]);
        // Nothing listening: a clear error.
        assert!(chat_messages_stream(1, &[("user", "x")], 1, &[], &mut |_| true).is_err());
    }

    #[test]
    fn escape_round_trip() {
        let s = "a\"b\\c\nd\u{1}é";
        let json = format!("{{\"k\":\"{}\"}}", json_escape(s));
        assert_eq!(json_get_string(&json, "k").unwrap(), s);
    }

    #[test]
    fn get_string_cases() {
        let j = r#"{"index":0, "content" : "hi é 😀", "x":1}"#;
        assert_eq!(json_get_string(j, "content").unwrap(), "hi é 😀");
        assert!(json_get_string(j, "missing").is_none());
        assert!(json_get_string(r#"{"k":"unterminated"#, "k").is_none());
        assert!(json_get_string(r#"{"k":1}"#, "k").is_none());
    }

    #[test]
    fn parse_status_and_body() {
        let r = "HTTP/1.1 200 OK\r\nContent-Type: x\r\n\r\n{\"a\":1}";
        assert_eq!(parse_response(r), Some((200, "{\"a\":1}")));
        assert_eq!(parse_response("HTTP/1.0 503 Busy\n\nno"), Some((503, "no")));
        assert_eq!(parse_response("garbage"), None);
    }

    /// Runs one request against a throwaway server and returns the body that was sent.
    fn body_sent(stop: &[String], stream: bool) -> String {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"}") {
                let n = c.read(&mut buf).unwrap();
                assert!(n > 0, "client closed early");
                got.extend_from_slice(&buf[..n]);
            }
            let reply: &[u8] = if stream {
                b"HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\n"
            } else {
                b"HTTP/1.0 200 OK\r\n\r\n{\"choices\":[{\"message\":{\"content\":\"x\"}}]}"
            };
            c.write_all(reply).unwrap();
            String::from_utf8(got).unwrap()
        });
        if stream {
            chat_messages_stream(port, &[("user", "hi")], 40, stop, &mut |_| true).unwrap();
        } else {
            chat_messages(port, &[("user", "hi")], 40, stop).unwrap();
        }
        t.join().unwrap()
    }

    #[test]
    fn stop_strings_are_sent_only_when_there_are_some_and_are_escaped() {
        for stream in [false, true] {
            let none = body_sent(&[], stream);
            assert!(
                !none.contains("\"stop\""),
                "no stop key without stop strings: {none}"
            );
            assert!(none.contains("\"max_tokens\":40"));
            let some = body_sent(&["END".to_string(), "a\nb\"c".to_string()], stream);
            assert!(some.contains("\"stop\":[\"END\",\"a\\nb\\\"c\"]"), "{some}");
            assert!(some.contains(&format!("\"stream\":{stream}")), "{some}");
            // The body is still one JSON object, ending in a closing brace.
            assert!(some.trim_end().ends_with('}'));
        }
    }

    #[test]
    fn chat_messages_sends_history_and_reads_reply() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"}") {
                let n = c.read(&mut buf).unwrap();
                assert!(n > 0, "client closed early");
                got.extend_from_slice(&buf[..n]);
            }
            c.write_all(
                b"HTTP/1.0 200 OK\r\n\r\n{\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"reasoning_content\":\"hmm\",\"content\":\"hi \\u00e9\\n!\"}}]}",
            )
            .unwrap();
            String::from_utf8(got).unwrap()
        });
        let r = chat_messages(
            port,
            &[("system", "be brief"), ("user", "say \"hi\"")],
            16,
            &[],
        )
        .unwrap();
        assert_eq!(r, "hi \u{e9}\n!");
        let sent = t.join().unwrap();
        assert!(sent.contains("POST /v1/chat/completions"));
        assert!(sent.contains("{\"role\":\"system\",\"content\":\"be brief\"},{\"role\":\"user\",\"content\":\"say \\\"hi\\\"\"}"));
        assert!(sent.contains("\"max_tokens\":16"));
    }

    #[test]
    fn request_against_local_listener() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"}") {
                let n = c.read(&mut buf).unwrap();
                assert!(n > 0, "client closed early");
                got.extend_from_slice(&buf[..n]);
            }
            c.write_all(b"HTTP/1.0 200 OK\r\n\r\n{\"content\":\"pong\"}")
                .unwrap();
        });
        assert_eq!(chat(port, "ping", 4).unwrap(), "pong");
        t.join().unwrap();
    }
}
