//! The chat request and its streamed reply.

use crate::Error;
use aeon_gateway::json::{self, obj, s, Value};
use aeon_http::{Client, Request, Url};
use std::io::{BufRead, BufReader, Read};

/// Longest reply accepted, in bytes.
const MAX_REPLY: usize = 1 << 20;
/// Longest single event line accepted.
const MAX_LINE: u64 = 1 << 20;
/// How much of an error body is read.
const MAX_ERROR_BODY: usize = 16 * 1024;

/// An API key. Does not print, and is overwritten when dropped (best effort: the allocator may
/// have copied it earlier).
pub struct Secret(String);

impl Secret {
    /// Wrap a key, refusing empty keys and anything that could break a header.
    ///
    /// # Errors
    /// [`Error::Config`] for an empty key or one with spaces or control characters.
    pub fn new(key: &str) -> Result<Self, Error> {
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::Config("the API key is empty"));
        }
        if key.bytes().any(|b| b <= b' ' || b == 0x7F || b >= 0x80) {
            return Err(Error::Config("the API key has spaces or odd characters"));
        }
        Ok(Self(key.to_string()))
    }

    /// The key text, for the `Authorization` header and for saving.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(***)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Overwrite in place, then let the String go.
        let n = self.0.len();
        self.0.clear();
        self.0.extend(std::iter::repeat_n('\0', n));
        self.0.clear();
    }
}

/// A hosted provider: where to send chats, which model, and the key.
#[derive(Debug)]
pub struct Provider {
    /// A short name, used for the key file.
    pub name: String,
    /// Base URL ending where `/chat/completions` is appended, e.g. `https://api.example.com/v1`.
    pub base_url: String,
    /// Model identifier.
    pub model: String,
    /// The API key.
    pub key: Secret,
}

/// Replace every occurrence of the key in `text`.
fn redact(text: &str, key: &Secret) -> String {
    text.replace(key.expose(), "[key removed]")
}

fn request_json(model: &str, turns: &[(&str, &str)], max_tokens: u32, stream: bool) -> String {
    let messages = turns
        .iter()
        .map(|(r, c)| obj(vec![("role", s(r)), ("content", s(c))]))
        .collect();
    obj(vec![
        ("model", s(model)),
        ("messages", Value::Arr(messages)),
        ("max_tokens", Value::Num(f64::from(max_tokens))),
        ("stream", Value::Bool(stream)),
    ])
    .to_json()
}

/// Send a chat and stream the reply: `on_token` is called with each piece of text as it
/// arrives, and the full reply is returned.
///
/// # Errors
/// [`Error`] for network, TLS, certificate, status and format problems. A reply that ends
/// before the provider says it is finished is an error, not a short answer.
pub fn chat_stream(
    client: &Client,
    provider: &Provider,
    turns: &[(&str, &str)],
    max_tokens: u32,
    on_token: &mut dyn FnMut(&str),
) -> Result<String, Error> {
    let url = Url::parse(&format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    ))?;
    let body = request_json(&provider.model, turns, max_tokens, true);
    let req = Request::post(url, "application/json", body.into_bytes())
        .header(
            "Authorization",
            &format!("Bearer {}", provider.key.expose()),
        )
        .header("Accept", "text/event-stream");
    let mut resp = client.request(&req)?;
    if resp.status != 200 {
        let status = resp.status;
        let raw = resp.read_all(MAX_ERROR_BODY).unwrap_or_default();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let message = json::parse(&text)
            .ok()
            .and_then(|v| match v.get("error")? {
                Value::Obj(_) => v.get("error")?.str("message").map(str::to_string),
                Value::Str(m) => Some(m.clone()),
                _ => None,
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(Error::Api {
            status,
            message: redact(&message, &provider.key),
        });
    }
    let is_json = resp
        .header("content-type")
        .is_some_and(|c| c.to_ascii_lowercase().starts_with("application/json"));
    if is_json {
        // The server ignored `stream` and sent the whole answer.
        let raw = resp.read_all(MAX_REPLY)?;
        let v = json::parse(&String::from_utf8_lossy(&raw))
            .map_err(|_| Error::BadReply("the reply is not valid JSON"))?;
        let text = first_choice(&v)
            .and_then(|c| c.get("message"))
            .and_then(|m| m.str("content"))
            .ok_or(Error::BadReply("no message in the reply"))?
            .to_string();
        on_token(&text);
        return Ok(text);
    }
    read_events(BufReader::new(&mut resp.body), on_token)
}

fn first_choice(v: &Value) -> Option<&Value> {
    match v.get("choices")? {
        Value::Arr(a) => a.first(),
        _ => None,
    }
}

/// Read server-sent events until `[DONE]` or a finish reason.
fn read_events<R: Read>(
    mut r: BufReader<R>,
    on_token: &mut dyn FnMut(&str),
) -> Result<String, Error> {
    let mut reply = String::new();
    let mut finished = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut r).take(MAX_LINE).read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            return Err(Error::BadReply("an event line is too long or cut off"));
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        let Some(data) = text.strip_prefix("data:") else {
            continue; // blank separators, comments, other fields
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            return Ok(reply);
        }
        let v = json::parse(data).map_err(|_| Error::BadReply("an event is not valid JSON"))?;
        if let Some(e) = v.get("error") {
            let m = match e {
                Value::Obj(_) => e.str("message").unwrap_or("error"),
                Value::Str(m) => m,
                _ => "error",
            };
            return Err(Error::Api {
                status: 200,
                message: m.chars().take(200).collect(),
            });
        }
        if let Some(choice) = first_choice(&v) {
            if let Some(piece) = choice.get("delta").and_then(|d| d.str("content")) {
                if reply.len() + piece.len() > MAX_REPLY {
                    return Err(Error::BadReply("the reply is too long"));
                }
                reply.push_str(piece);
                on_token(piece);
            }
            if matches!(choice.get("finish_reason"), Some(Value::Str(_))) {
                finished = true;
            }
        }
    }
    if finished {
        Ok(reply)
    } else {
        Err(Error::BadReply(
            "the reply ended before the provider finished it",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_validate_and_hide() {
        assert!(Secret::new("").is_err());
        assert!(Secret::new("a b").is_err());
        assert!(Secret::new("a\r\nX: y").is_err());
        let k = Secret::new("  sk-abc123\n").unwrap();
        assert_eq!(k.expose(), "sk-abc123");
        assert_eq!(format!("{k:?}"), "Secret(***)");
        let p = Provider {
            name: "p".into(),
            base_url: "u".into(),
            model: "m".into(),
            key: k,
        };
        assert!(!format!("{p:?}").contains("abc123"));
    }

    #[test]
    fn request_body_has_no_key_and_escapes_text() {
        let body = request_json(
            "m1",
            &[("system", "be \"nice\""), ("user", "line1\nline2")],
            64,
            true,
        );
        let v = json::parse(&body).unwrap();
        assert_eq!(v.str("model"), Some("m1"));
        assert_eq!(v.bool("stream"), Some(true));
        assert!(body.contains("\\n") && body.contains("\\\"nice\\\""));
    }

    #[test]
    fn events_stream_tokens_and_decode_escapes() {
        let sse = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                   : keep-alive\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"lo \\u00e9 \\ud83d\\ude00\"}}]}\r\n\r\n\
                   data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
        let mut seen = Vec::new();
        let r = read_events(BufReader::new(sse.as_bytes()), &mut |t| {
            seen.push(t.to_string())
        })
        .unwrap();
        assert_eq!(r, "Hello é 😀");
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn early_end_and_garbage_are_errors() {
        let cut = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n";
        assert!(read_events(BufReader::new(cut.as_bytes()), &mut |_| {}).is_err());
        let bad = "data: {not json}\n\n";
        assert!(read_events(BufReader::new(bad.as_bytes()), &mut |_| {}).is_err());
        let err = "data: {\"error\":{\"message\":\"overloaded\"}}\n\n";
        assert!(matches!(
            read_events(BufReader::new(err.as_bytes()), &mut |_| {}),
            Err(Error::Api { .. })
        ));
        // finish_reason without [DONE] is accepted
        let ok =
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\n";
        assert_eq!(
            read_events(BufReader::new(ok.as_bytes()), &mut |_| {}).unwrap(),
            "x"
        );
    }

    #[test]
    fn redaction_removes_the_key() {
        let k = Secret::new("sk-secret").unwrap();
        assert_eq!(
            redact("bad key sk-secret here", &k),
            "bad key [key removed] here"
        );
    }
}
