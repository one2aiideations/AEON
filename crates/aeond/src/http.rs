//! Minimal HTTP/1.0 client and flat-JSON helpers for the local inference server.

use std::io::{Read, Write};
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
    let (status, resp) = request(port, "POST", "/completion", Some(&body), Duration::from_secs(120))
        .map_err(|e| format!("request failed: {e}"))?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    json_get_string(&resp, "content").ok_or_else(|| "no content in reply".into())
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
