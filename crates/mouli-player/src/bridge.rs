//! The bridge to YouTube's own player.
//!
//! YouTube may only be shown by YouTube's official player (the IFrame API), which needs a browser
//! engine. This module serves a tiny page on this computer (`127.0.0.1`) that embeds that player and
//! talks to the engine: the engine's commands go out to the page by long polling, and the page
//! reports ended, error, position and so on back with small POSTs. The browser engine itself
//! (for example WPE WebKit with `cog`) is added by the OS image and pointed at [`Bridge::page_url`].
//!
//! The server is locked down: every path starts with a secret per-run token, the `Host` header must
//! be this server (against DNS rebinding), a POST must be JSON from this server's own origin (so a
//! web page elsewhere cannot send one), bodies are tiny, and the page may only load scripts from
//! itself and YouTube. Nothing here fetches or decodes YouTube video.

use crate::engine::{Backend, Event};
use crate::mpv::{parse_json, quote, Json};
use crate::source::Source;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Largest request header block accepted.
pub const MAX_HEADER: usize = 8192;
/// Largest request body accepted.
pub const MAX_BODY: usize = 1024;
/// Most connections served at once.
pub const MAX_CONNECTIONS: usize = 8;
/// Longest a command poll may wait, in seconds.
pub const MAX_WAIT: u64 = 25;
const MAX_EVENTS: usize = 256;
/// The page counts as connected if it polled this recently.
const CONNECTED_WITHIN: Duration = Duration::from_secs(30);

const PLAYER_JS: &str = include_str!("player.js");

#[derive(Default)]
struct Shared {
    commands: VecDeque<String>,
    events: VecDeque<Event>,
    last_poll: Option<Instant>,
}

type State = Arc<(Mutex<Shared>, Condvar)>;

/// A secret for one run: 32 hex characters from the system's random source.
///
/// # Errors
/// The I/O error when `/dev/urandom` cannot be read.
pub fn new_token() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Compares in time that does not depend on where the texts differ.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(1));
    }
    diff == 0
}

/// How many seconds a command poll may wait: the page's `wait=` value, 15 when it is missing or
/// not a plain number, and never more than [`MAX_WAIT`] (a number too big to read counts as the
/// most).
pub fn parse_wait(query: &str) -> u64 {
    let Some(v) = query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == "wait")
        .map(|(_, v)| v)
    else {
        return 15;
    };
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return 15;
    }
    v.parse::<u64>().unwrap_or(u64::MAX).min(MAX_WAIT)
}

/// The message to show for a YouTube player error code.
pub fn youtube_error(code: i64) -> String {
    match code {
        2 => "YouTube says the video address is invalid".into(),
        5 => "YouTube's player had an internal error".into(),
        100 => "The video was removed or is private".into(),
        101 | 150 => "The owner does not allow this video to play outside YouTube".into(),
        153 => "YouTube could not tell where the player runs (error 153)".into(),
        c => format!("YouTube player error {c}"),
    }
}

fn number(j: &Json, key: &str) -> Option<u32> {
    let n = j.get(key)?.as_f64()?;
    (n.is_finite() && (0.0..=1e7).contains(&n)).then_some(n as u32)
}

/// Turns the page's report into a player event.
///
/// # Errors
/// A message when the report is not one the page sends.
pub fn event_from_json(body: &str) -> Result<Event, String> {
    let j = parse_json(body)?;
    let kind = j
        .get("event")
        .and_then(Json::as_str)
        .ok_or("missing event")?;
    Ok(match kind {
        "loaded" => Event::Loaded(number(&j, "duration")),
        "playing" => Event::Playing,
        "paused" => Event::Paused,
        "ended" => Event::Ended,
        "position" => Event::Position(number(&j, "secs").ok_or("missing secs")?),
        "error" => {
            let code = j
                .get("code")
                .and_then(Json::as_f64)
                .filter(|c| c.is_finite() && c.fract() == 0.0)
                .map_or(-1, |c| c as i64);
            Event::Error(youtube_error(code))
        }
        other => return Err(format!("unknown event {other}")),
    })
}

struct Request {
    method: String,
    path: String,
    query: String,
    host: String,
    origin: Option<String>,
    content_type: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request, (u16, &'static str)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER {
            return Err((431, "headers too large"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err((400, "closed")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return Err((408, "timeout")),
        }
    };
    // The limit also applies when the whole block arrived in one piece.
    if end > MAX_HEADER {
        return Err((431, "headers too large"));
    }
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| (400, "bad header text"))?;
    let mut lines = head.split("\r\n");
    let first = lines.next().ok_or((400, "empty"))?;
    let mut parts = first.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    if method.is_empty()
        || !target.starts_with('/')
        || !version.starts_with("HTTP/1.")
        || parts.next().is_some()
    {
        return Err((400, "bad request line"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (mut host, mut origin, mut content_type) = (String::new(), None, String::new());
    let mut length = 0usize;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            return Err((400, "bad header"));
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "host" => host = v.to_ascii_lowercase(),
            "origin" => origin = Some(v.to_ascii_lowercase()),
            "content-type" => content_type = v.to_ascii_lowercase(),
            "content-length" => length = v.parse().map_err(|_| (400, "bad length"))?,
            "transfer-encoding" | "expect" | "upgrade" => return Err((501, "not supported")),
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Err((413, "body too large"));
    }
    let mut body = buf[end + 4..].to_vec();
    if body.len() > length {
        return Err((400, "extra data"));
    }
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) => return Err((400, "closed")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Err((408, "timeout")),
        }
        if body.len() > length {
            return Err((400, "extra data"));
        }
    }
    Ok(Request {
        method: method.to_string(),
        path: path.to_string(),
        query: query.to_string(),
        host,
        origin,
        content_type,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, extra: &[(&str, &str)], body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        _ => "Service Unavailable",
    };
    let mut head = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n", body.len());
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// Ends a connection cleanly: stop sending, then read and discard what the client is still
/// sending (a little, and only briefly). Closing with unread data would make the operating system
/// reset the connection and throw away the answer that was just written.
fn finish(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let mut sink = [0u8; 4096];
    let mut total = 0usize;
    while total < 128 * 1024 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

fn page_html(base: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MOULI player</title>\
<style>html,body,#p{{margin:0;width:100%;height:100%;background:#000}}</style></head>\
<body><div id=\"p\"></div><script src=\"{base}/player.js\"></script>\
<script src=\"https://www.youtube.com/iframe_api\"></script></body></html>"
    )
}

struct Server {
    state: State,
    token: String,
    port: u16,
    io_timeout: Duration,
}

impl Server {
    fn allowed_hosts(&self) -> [String; 2] {
        [
            format!("127.0.0.1:{}", self.port),
            format!("localhost:{}", self.port),
        ]
    }

    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(self.io_timeout));
        let _ = stream.set_write_timeout(Some(self.io_timeout));
        self.serve(&mut stream);
        finish(&mut stream);
    }

    fn serve(&self, stream: &mut TcpStream) {
        let req = match read_request(stream) {
            Ok(r) => r,
            Err((code, _)) => return respond(stream, code, &[], b""),
        };
        if !self.allowed_hosts().contains(&req.host) {
            return respond(stream, 421, &[], b"");
        }
        // Every path is /t/<token>/<route>. The token is compared in constant time, and a wrong
        // or missing token looks the same as any other page that does not exist.
        let Some(rest) = req.path.strip_prefix("/t/") else {
            return respond(stream, 404, &[], b"");
        };
        let (given, route) = rest.split_once('/').unwrap_or((rest, ""));
        if !same(given, &self.token) {
            return respond(stream, 404, &[], b"");
        }
        match (req.method.as_str(), route) {
            ("GET", "player") => {
                let csp = "default-src 'none'; script-src 'self' https://www.youtube.com https://s.ytimg.com; frame-src https://www.youtube.com https://www.youtube-nocookie.com; connect-src 'self'; style-src 'unsafe-inline'; img-src https://i.ytimg.com data:; base-uri 'none'; form-action 'none'";
                let html = page_html(&format!("/t/{}", self.token));
                respond(
                    stream,
                    200,
                    &[
                        ("Content-Type", "text/html; charset=utf-8"),
                        ("Content-Security-Policy", csp),
                        ("Referrer-Policy", "strict-origin-when-cross-origin"),
                    ],
                    html.as_bytes(),
                );
            }
            ("GET", "player.js") => respond(
                stream,
                200,
                &[("Content-Type", "text/javascript; charset=utf-8")],
                PLAYER_JS.as_bytes(),
            ),
            ("GET", "cmd") => self.poll(stream, &req.query),
            ("POST", "event") => self.event(stream, &req),
            (_, "player" | "player.js" | "cmd" | "event") => respond(stream, 405, &[], b""),
            _ => respond(stream, 404, &[], b""),
        }
    }

    fn poll(&self, stream: &mut TcpStream, query: &str) {
        let wait = parse_wait(query);
        let deadline = Instant::now() + Duration::from_secs(wait);
        let (lock, cv) = &*self.state;
        let mut g = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.last_poll = Some(Instant::now());
        let out = loop {
            if let Some(c) = g.commands.pop_front() {
                break c;
            }
            let now = Instant::now();
            if now >= deadline {
                break "{\"op\":\"none\"}".to_string();
            }
            g = cv
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        };
        drop(g);
        respond(
            stream,
            200,
            &[("Content-Type", "application/json")],
            out.as_bytes(),
        );
    }

    fn event(&self, stream: &mut TcpStream, req: &Request) {
        // A browser sends this header on every cross-origin request; only our own page may post.
        if let Some(o) = &req.origin {
            if !self
                .allowed_hosts()
                .iter()
                .any(|h| *o == format!("http://{h}"))
            {
                return respond(stream, 403, &[], b"");
            }
        }
        if !req.content_type.starts_with("application/json") {
            return respond(stream, 415, &[], b"");
        }
        let Ok(text) = std::str::from_utf8(&req.body) else {
            return respond(stream, 400, &[], b"");
        };
        match event_from_json(text) {
            Ok(ev) => {
                let (lock, _) = &*self.state;
                let mut g = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if g.events.len() >= MAX_EVENTS {
                    g.events.pop_front();
                }
                g.events.push_back(ev);
                drop(g);
                respond(stream, 200, &[("Content-Type", "application/json")], b"{}");
            }
            Err(_) => respond(stream, 400, &[], b""),
        }
    }
}

struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The running bridge server.
pub struct Bridge {
    state: State,
    addr: SocketAddr,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Bridge {
    /// Starts the server on a free port of `127.0.0.1`, using `token` as the secret in every path.
    ///
    /// # Errors
    /// The I/O error when the port cannot be bound.
    pub fn start(token: &str) -> std::io::Result<Self> {
        Self::start_with_timeout(token, Duration::from_secs(5))
    }

    /// Like [`Bridge::start`] with a chosen read and write timeout per connection.
    ///
    /// # Errors
    /// The I/O error when the port cannot be bound.
    pub fn start_with_timeout(token: &str, io_timeout: Duration) -> std::io::Result<Self> {
        if token.len() < 16 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the token must be at least 16 letters or digits",
            ));
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let state: State = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let server = Arc::new(Server {
            state: state.clone(),
            token: token.to_string(),
            port: addr.port(),
            io_timeout,
        });
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                            active.fetch_sub(1, Ordering::SeqCst);
                            let mut s = stream;
                            respond(&mut s, 503, &[], b"");
                            let _ = s.shutdown(Shutdown::Both);
                            continue;
                        }
                        let (srv, guard) = (server.clone(), Guard(active.clone()));
                        std::thread::spawn(move || {
                            let _guard = guard;
                            srv.handle(stream);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Ok(Self {
            state,
            addr,
            token: token.to_string(),
            stop,
            thread: Some(thread),
        })
    }

    /// The address to open in the browser engine.
    pub fn page_url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/t/{}/player",
            self.addr.port(),
            self.token
        )
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// True when the page has polled for commands in the last 30 seconds.
    pub fn connected(&self) -> bool {
        let g = self
            .state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.last_poll.is_some_and(|t| t.elapsed() < CONNECTED_WITHIN)
    }

    /// Takes everything the page has reported since the last call.
    pub fn drain_events(&self) -> Vec<Event> {
        let mut g = self
            .state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.events.drain(..).collect()
    }

    /// A [`Backend`] that sends the engine's commands to the page.
    pub fn backend(&self) -> YouTubeBackend {
        YouTubeBackend {
            state: self.state.clone(),
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The engine's side of the bridge: it queues commands for the page to pick up.
pub struct YouTubeBackend {
    state: State,
}

impl YouTubeBackend {
    fn send(&self, json: String) {
        let (lock, cv) = &*self.state;
        let mut g = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A page that stopped polling must not make the queue grow without end.
        if g.commands.len() >= MAX_EVENTS {
            g.commands.pop_front();
        }
        g.commands.push_back(json);
        cv.notify_all();
    }
}

impl Backend for YouTubeBackend {
    fn load(&mut self, source: &Source) -> Result<(), String> {
        match source {
            Source::YouTube { id, .. } => {
                self.send(format!("{{\"op\":\"load\",\"id\":{}}}", quote(id)));
                Ok(())
            }
            _ => Err("only YouTube items play in YouTube's player".into()),
        }
    }

    fn pause(&mut self, paused: bool) {
        self.send(format!(
            "{{\"op\":\"{}\"}}",
            if paused { "pause" } else { "play" }
        ));
    }

    fn seek(&mut self, secs: u32) {
        self.send(format!("{{\"op\":\"seek\",\"secs\":{secs}}}"));
    }

    fn set_volume(&mut self, percent: u8) {
        self.send(format!(
            "{{\"op\":\"volume\",\"value\":{}}}",
            percent.min(100)
        ));
    }

    fn set_speed(&mut self, percent: u16) {
        self.send(format!(
            "{{\"op\":\"rate\",\"value\":{}.{:02}}}",
            percent / 100,
            percent % 100
        ));
    }

    fn stop(&mut self) {
        self.send("{\"op\":\"stop\"}".to_string());
    }
}

/// Sends YouTube items to one backend and everything else to another.
pub struct Router<M: Backend, Y: Backend> {
    /// The backend for files and direct URLs (mpv).
    pub local: M,
    /// The backend for YouTube items.
    pub youtube: Y,
    active: Option<bool>,
}

impl<M: Backend, Y: Backend> Router<M, Y> {
    /// Combines the two backends.
    pub fn new(local: M, youtube: Y) -> Self {
        Self {
            local,
            youtube,
            active: None,
        }
    }
}

impl<M: Backend, Y: Backend> Backend for Router<M, Y> {
    fn load(&mut self, source: &Source) -> Result<(), String> {
        let yt = source.is_youtube();
        // Starting something on the other backend stops what played before.
        if self.active == Some(!yt) {
            if yt {
                self.local.stop();
            } else {
                self.youtube.stop();
            }
        }
        let r = if yt {
            self.youtube.load(source)
        } else {
            self.local.load(source)
        };
        if r.is_ok() {
            self.active = Some(yt);
        }
        r
    }

    fn pause(&mut self, paused: bool) {
        match self.active {
            Some(true) => self.youtube.pause(paused),
            Some(false) => self.local.pause(paused),
            None => {}
        }
    }

    fn seek(&mut self, secs: u32) {
        match self.active {
            Some(true) => self.youtube.seek(secs),
            Some(false) => self.local.seek(secs),
            None => {}
        }
    }

    fn set_volume(&mut self, percent: u8) {
        self.local.set_volume(percent);
        self.youtube.set_volume(percent);
    }

    fn set_speed(&mut self, percent: u16) {
        self.local.set_speed(percent);
        self.youtube.set_speed(percent);
    }

    fn stop(&mut self) {
        self.local.stop();
        self.youtube.stop();
        self.active = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Command, Player, State};
    use crate::queue::Item;
    use crate::source::YtKind;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn bridge() -> Bridge {
        Bridge::start_with_timeout(TOKEN, Duration::from_millis(500)).unwrap()
    }

    struct Resp {
        status: u16,
        head: String,
        body: String,
    }

    fn raw(port: u16, req: &[u8]) -> Resp {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        s.write_all(req).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let text = String::from_utf8_lossy(&out).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        Resp {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    fn get_with(port: u16, path: &str, host: &str) -> Resp {
        raw(
            port,
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
        )
    }

    fn get(b: &Bridge, path: &str) -> Resp {
        get_with(b.port(), path, &format!("127.0.0.1:{}", b.port()))
    }

    fn post_with(b: &Bridge, path: &str, body: &str, ctype: &str, origin: Option<&str>) -> Resp {
        let o = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        raw(
            b.port(),
            format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: {ctype}\r\n{o}Content-Length: {}\r\n\r\n{body}", b.port(), body.len()).as_bytes(),
        )
    }

    fn post(b: &Bridge, body: &str) -> Resp {
        post_with(
            b,
            &format!("/t/{TOKEN}/event"),
            body,
            "application/json",
            None,
        )
    }

    #[test]
    fn tokens_are_random_hex_and_comparison_is_exact() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(
            same("abc", "abc")
                && !same("abc", "abd")
                && !same("abc", "ab")
                && !same("ab", "abc")
                && !same("", "a")
                && same("", "")
        );
    }

    #[test]
    fn a_weak_token_is_refused() {
        for t in [
            "",
            "short",
            "has space 0123456789abc",
            "has/slash0123456789abc",
        ] {
            assert!(Bridge::start(t).is_err(), "{t:?}");
        }
    }

    #[test]
    fn the_player_page_and_script_are_served_with_a_strict_policy() {
        let b = bridge();
        let r = get(&b, &format!("/t/{TOKEN}/player"));
        assert_eq!(r.status, 200);
        assert!(r.head.contains("text/html"));
        assert!(
            r.head
                .contains("script-src 'self' https://www.youtube.com https://s.ytimg.com"),
            "{}",
            r.head
        );
        assert!(!r.head.contains("unsafe-eval") && !r.head.contains("script-src 'unsafe-inline'"));
        assert!(
            r.head.contains("default-src 'none'")
                && r.head.contains("connect-src 'self'")
                && r.head.contains("frame-src https://www.youtube.com")
        );
        assert!(
            r.head
                .contains("Referrer-Policy: strict-origin-when-cross-origin")
                && r.head.contains("nosniff")
                && r.head.contains("no-store")
        );
        assert!(
            r.body.contains(&format!("/t/{TOKEN}/player.js"))
                && r.body.contains("https://www.youtube.com/iframe_api")
        );
        assert!(!r.body.contains("<script>"), "no inline script");
        let js = get(&b, &format!("/t/{TOKEN}/player.js"));
        assert_eq!(js.status, 200);
        assert!(js.head.contains("text/javascript"));
        assert_eq!(js.body, PLAYER_JS);
        assert!(
            b.page_url().starts_with("http://127.0.0.1:")
                && b.page_url().ends_with(&format!("/t/{TOKEN}/player"))
        );
    }

    #[test]
    fn a_wrong_or_missing_token_and_odd_paths_all_look_like_nothing_is_there() {
        let b = bridge();
        for p in [
            "/player".to_string(),
            "/t/".to_string(),
            "/t".to_string(),
            "/".to_string(),
            "/t/wrongtoken0123456789abcdef0000/player".to_string(),
            format!("/t/{TOKEN}0/player"),
            format!("/t/{}/player", &TOKEN[..31]),
            format!("/t/{TOKEN}/../player"),
            format!("/t/{TOKEN}/player/../../x"),
            format!("/t/{TOKEN}//player"),
            format!("/t/{TOKEN}/nothing"),
            format!("/t/{TOKEN}"),
            format!("/T/{TOKEN}/player"),
        ] {
            let r = get(&b, &p);
            assert_eq!((r.status, r.body.as_str()), (404, ""), "{p}");
        }
    }

    #[test]
    fn requests_for_another_host_name_are_refused_against_dns_rebinding() {
        let b = bridge();
        let p = format!("/t/{TOKEN}/player");
        let port = b.port();
        assert_eq!(get_with(port, &p, "evil.example").status, 421);
        assert_eq!(
            get_with(port, &p, &format!("evil.example:{port}")).status,
            421
        );
        assert_eq!(
            get_with(port, &p, "127.0.0.1").status,
            421,
            "the port must be there"
        );
        assert_eq!(
            get_with(port, &p, &format!("127.0.0.1:{}", port + 1)).status,
            421
        );
        assert_eq!(
            raw(port, format!("GET {p} HTTP/1.1\r\n\r\n").as_bytes()).status,
            421,
            "no Host header"
        );
        assert_eq!(get_with(port, &p, &format!("localhost:{port}")).status, 200);
        assert_eq!(get_with(port, &p, &format!("LOCALHOST:{port}")).status, 200);
    }

    #[test]
    fn only_the_right_methods_work_on_each_route() {
        let b = bridge();
        let port = b.port();
        let host = format!("Host: 127.0.0.1:{port}");
        for (m, route, want) in [
            ("DELETE", "player", 405),
            ("POST", "player", 405),
            ("PUT", "event", 405),
            ("GET", "event", 405),
            ("POST", "cmd", 405),
            ("POST", "player.js", 405),
            ("OPTIONS", "event", 405),
            ("HEAD", "player", 405),
        ] {
            let r = raw(
                port,
                format!("{m} /t/{TOKEN}/{route} HTTP/1.1\r\n{host}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            );
            assert_eq!(r.status, want, "{m} {route}");
        }
    }

    #[test]
    fn what_the_page_reports_becomes_player_events_in_order() {
        let b = bridge();
        for body in [
            "{\"event\":\"loaded\",\"duration\":31}",
            "{\"event\":\"playing\"}",
            "{\"event\":\"position\",\"secs\":5}",
            "{\"event\":\"paused\"}",
            "{\"event\":\"ended\"}",
            "{\"event\":\"error\",\"code\":100}",
            "{\"event\":\"loaded\"}",
        ] {
            assert_eq!(post(&b, body).status, 200, "{body}");
        }
        assert_eq!(
            b.drain_events(),
            vec![
                Event::Loaded(Some(31)),
                Event::Playing,
                Event::Position(5),
                Event::Paused,
                Event::Ended,
                Event::Error("The video was removed or is private".into()),
                Event::Loaded(None)
            ]
        );
        assert!(b.drain_events().is_empty(), "taken once");
    }

    #[test]
    fn bad_reports_wrong_types_foreign_origins_and_big_bodies_are_refused_and_change_nothing() {
        let b = bridge();
        let ev = format!("/t/{TOKEN}/event");
        for (body, want) in [
            ("not json", 400),
            ("{}", 400),
            ("{\"event\":\"explode\"}", 400),
            ("{\"event\":\"position\"}", 400),
            ("{\"event\":\"position\",\"secs\":-1}", 400),
            ("{\"event\":\"position\",\"secs\":1e12}", 400),
            ("{\"event\":5}", 400),
            ("[1]", 400),
        ] {
            assert_eq!(post(&b, body).status, want, "{body}");
        }
        assert_eq!(
            post_with(&b, &ev, "{\"event\":\"ended\"}", "text/plain", None).status,
            415
        );
        assert_eq!(
            post_with(
                &b,
                &ev,
                "{\"event\":\"ended\"}",
                "application/x-www-form-urlencoded",
                None
            )
            .status,
            415
        );
        for origin in [
            "http://evil.example",
            "null",
            "https://127.0.0.1",
            "http://127.0.0.1:1",
        ] {
            assert_eq!(
                post_with(
                    &b,
                    &ev,
                    "{\"event\":\"ended\"}",
                    "application/json",
                    Some(origin)
                )
                .status,
                403,
                "{origin}"
            );
        }
        let big = format!(
            "{{\"event\":\"ended\",\"pad\":\"{}\"}}",
            "x".repeat(MAX_BODY)
        );
        assert_eq!(post(&b, &big).status, 413);
        let r = raw(b.port(), format!("POST {ev} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: 999999\r\n\r\n", b.port()).as_bytes());
        assert_eq!(r.status, 413);
        assert!(b.drain_events().is_empty(), "nothing got through");
        let own = format!("http://127.0.0.1:{}", b.port());
        assert_eq!(
            post_with(
                &b,
                &ev,
                "{\"event\":\"ended\"}",
                "application/json; charset=utf-8",
                Some(&own)
            )
            .status,
            200
        );
        assert_eq!(b.drain_events(), vec![Event::Ended]);
    }

    #[test]
    fn malformed_http_is_rejected_without_a_crash() {
        let b = bridge();
        let port = b.port();
        let host = format!("Host: 127.0.0.1:{port}");
        assert_eq!(
            raw(
                port,
                format!(
                    "GET /t/{TOKEN}/cmd HTTP/1.1\r\n{host}\r\nTransfer-Encoding: chunked\r\n\r\n"
                )
                .as_bytes()
            )
            .status,
            501
        );
        assert_eq!(
            raw(
                port,
                format!("GET /t/{TOKEN}/cmd HTTP/1.1\r\n{host}\r\nUpgrade: websocket\r\n\r\n")
                    .as_bytes()
            )
            .status,
            501
        );
        assert_eq!(
            raw(
                port,
                format!("GET /t/{TOKEN}/cmd HTTP/1.1\r\n{host}\r\nbadheader\r\n\r\n").as_bytes()
            )
            .status,
            400
        );
        assert_eq!(raw(port, b"GARBAGE\r\n\r\n").status, 400);
        assert_eq!(raw(port, b"GET nothing HTTP/1.1\r\n\r\n").status, 400);
        assert_eq!(
            raw(
                port,
                format!("GET /t/{TOKEN}/cmd HTTP/2.0\r\n{host}\r\n\r\n").as_bytes()
            )
            .status,
            400
        );
        assert_eq!(
            raw(
                port,
                format!("GET /t/{TOKEN}/cmd HTTP/1.1 extra\r\n{host}\r\n\r\n").as_bytes()
            )
            .status,
            400
        );
        assert_eq!(
            raw(
                port,
                format!("POST /t/{TOKEN}/event HTTP/1.1\r\n{host}\r\nContent-Length: abc\r\n\r\n")
                    .as_bytes()
            )
            .status,
            400
        );
        let huge = format!(
            "GET /t/{TOKEN}/cmd HTTP/1.1\r\n{host}\r\nX-Pad: {}\r\n\r\n",
            "a".repeat(MAX_HEADER)
        );
        assert_eq!(raw(port, huge.as_bytes()).status, 431);
        assert_eq!(raw(port, b"\xff\xfe\r\n\r\n").status, 400);
        assert_eq!(
            get(&b, &format!("/t/{TOKEN}/player.js")).status,
            200,
            "still serving"
        );
    }

    #[test]
    fn commands_reach_the_page_in_order_and_an_empty_poll_says_none() {
        let b = bridge();
        let mut y = b.backend();
        y.load(&Source::YouTube {
            id: "dQw4w9WgXcQ".into(),
            kind: YtKind::Short,
        })
        .unwrap();
        y.pause(true);
        y.pause(false);
        y.seek(30);
        y.set_volume(250);
        y.set_speed(125);
        y.stop();
        let mut got = Vec::new();
        for _ in 0..8 {
            let r = get(&b, &format!("/t/{TOKEN}/cmd?wait=0"));
            assert_eq!(r.status, 200);
            assert!(r.head.contains("application/json"));
            got.push(r.body);
        }
        assert_eq!(
            got,
            vec![
                "{\"op\":\"load\",\"id\":\"dQw4w9WgXcQ\"}",
                "{\"op\":\"pause\"}",
                "{\"op\":\"play\"}",
                "{\"op\":\"seek\",\"secs\":30}",
                "{\"op\":\"volume\",\"value\":100}",
                "{\"op\":\"rate\",\"value\":1.25}",
                "{\"op\":\"stop\"}",
                "{\"op\":\"none\"}",
            ]
        );
        for line in &got {
            assert!(parse_json(line).is_ok(), "valid JSON: {line}");
        }
    }

    #[test]
    fn a_waiting_poll_returns_the_moment_a_command_arrives_and_gives_up_after_its_wait() {
        let b = bridge();
        let port = b.port();
        let t = Instant::now();
        let r = get_with(
            port,
            &format!("/t/{TOKEN}/cmd?wait=1"),
            &format!("127.0.0.1:{port}"),
        );
        assert_eq!(r.body, "{\"op\":\"none\"}");
        assert!(
            t.elapsed() >= Duration::from_millis(900) && t.elapsed() < Duration::from_secs(4),
            "{:?}",
            t.elapsed()
        );
        let h = std::thread::spawn(move || {
            let t = Instant::now();
            let r = get_with(
                port,
                &format!("/t/{TOKEN}/cmd?wait=10"),
                &format!("127.0.0.1:{port}"),
            );
            (r.body, t.elapsed())
        });
        std::thread::sleep(Duration::from_millis(200));
        b.backend().pause(true);
        let (body, took) = h.join().unwrap();
        assert_eq!(body, "{\"op\":\"pause\"}");
        assert!(took < Duration::from_secs(3), "{took:?}");
        for (q, want) in [
            ("", 15),
            ("wait=3", 3),
            ("wait=0", 0),
            ("wait=999", MAX_WAIT),
            ("wait=99999999999999999999", MAX_WAIT),
            ("wait=abc", 15),
            ("wait=-1", 15),
            ("wait=", 15),
            ("wait=2x", 15),
            ("x=1&wait=2", 2),
            ("wait=2&wait=9", 2),
        ] {
            assert_eq!(parse_wait(q), want, "{q:?}");
        }
    }

    #[test]
    fn the_bridge_knows_whether_the_page_is_there() {
        let b = bridge();
        assert!(!b.connected());
        get(&b, &format!("/t/{TOKEN}/cmd?wait=0"));
        assert!(b.connected());
    }

    #[test]
    fn the_command_queue_cannot_grow_without_end_when_nobody_polls() {
        let b = bridge();
        let mut y = b.backend();
        for i in 0..1000u32 {
            y.seek(i);
        }
        let mut n = 0;
        while get(&b, &format!("/t/{TOKEN}/cmd?wait=0")).body != "{\"op\":\"none\"}" {
            n += 1;
            assert!(n <= MAX_EVENTS, "more than the limit");
        }
        assert_eq!(n, MAX_EVENTS);
    }

    #[test]
    fn the_backend_refuses_what_is_not_youtube() {
        let b = bridge();
        let mut y = b.backend();
        assert!(y.load(&Source::File("/a.mp4".into())).is_err());
        assert!(y.load(&Source::Url("https://x.org/a.mp4".into())).is_err());
        assert_eq!(
            get(&b, &format!("/t/{TOKEN}/cmd?wait=0")).body,
            "{\"op\":\"none\"}"
        );
    }

    #[test]
    fn youtube_error_codes_get_plain_messages() {
        assert!(youtube_error(100).contains("removed or is private"));
        assert!(
            youtube_error(101).contains("does not allow")
                && youtube_error(150) == youtube_error(101)
        );
        assert!(youtube_error(153).contains("153"));
        assert_eq!(youtube_error(77), "YouTube player error 77");
        assert_eq!(
            event_from_json("{\"event\":\"error\"}").unwrap(),
            Event::Error("YouTube player error -1".into())
        );
        assert_eq!(
            event_from_json("{\"event\":\"error\",\"code\":2.5}").unwrap(),
            Event::Error("YouTube player error -1".into())
        );
    }

    #[test]
    fn too_many_connections_are_turned_away_and_service_resumes() {
        let b = Bridge::start_with_timeout(TOKEN, Duration::from_secs(4)).unwrap();
        let idle: Vec<TcpStream> = (0..MAX_CONNECTIONS)
            .map(|_| TcpStream::connect(("127.0.0.1", b.port())).unwrap())
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        let r = get(&b, &format!("/t/{TOKEN}/player.js"));
        assert_eq!(r.status, 503, "the ninth is refused while eight sit idle");
        drop(idle);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(get(&b, &format!("/t/{TOKEN}/player.js")).status, 200);
    }

    #[test]
    fn a_client_that_sends_nothing_or_trickles_is_dropped_after_the_timeout() {
        let b = bridge();
        let t = Instant::now();
        let mut s = TcpStream::connect(("127.0.0.1", b.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(b"GET /t/").unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 408") || out.is_empty());
        assert_eq!(get(&b, &format!("/t/{TOKEN}/player.js")).status, 200);
    }

    #[test]
    fn stopping_the_bridge_frees_its_port() {
        let b = bridge();
        let port = b.port();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        drop(b);
        let t = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(t.elapsed() < Duration::from_secs(3), "port still open");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[derive(Default)]
    struct Rec {
        log: Vec<String>,
    }
    impl Backend for Rec {
        fn load(&mut self, s: &Source) -> Result<(), String> {
            self.log.push(format!("load {}", s.label()));
            Ok(())
        }
        fn pause(&mut self, p: bool) {
            self.log.push(format!("pause {p}"));
        }
        fn seek(&mut self, s: u32) {
            self.log.push(format!("seek {s}"));
        }
        fn set_volume(&mut self, v: u8) {
            self.log.push(format!("vol {v}"));
        }
        fn set_speed(&mut self, v: u16) {
            self.log.push(format!("speed {v}"));
        }
        fn stop(&mut self) {
            self.log.push("stop".into());
        }
    }

    #[test]
    fn the_router_sends_each_item_to_its_backend_and_stops_what_played_before() {
        let mut r = Router::new(Rec::default(), Rec::default());
        r.pause(true);
        r.seek(3);
        assert!(
            r.local.log.is_empty() && r.youtube.log.is_empty(),
            "nothing active yet"
        );
        r.load(&Source::File("/m/a.mp4".into())).unwrap();
        r.pause(true);
        r.seek(7);
        r.load(&Source::YouTube {
            id: "dQw4w9WgXcQ".into(),
            kind: YtKind::Short,
        })
        .unwrap();
        r.pause(false);
        r.set_volume(40);
        r.set_speed(150);
        assert_eq!(
            r.local.log,
            vec![
                "load a.mp4",
                "pause true",
                "seek 7",
                "stop",
                "vol 40",
                "speed 150"
            ]
        );
        assert_eq!(
            r.youtube.log,
            vec![
                "load YouTube Short dQw4w9WgXcQ",
                "pause false",
                "vol 40",
                "speed 150"
            ]
        );
        r.load(&Source::File("/m/b.mp4".into())).unwrap();
        assert_eq!(r.youtube.log.last().unwrap(), "stop");
        r.stop();
        assert_eq!(r.local.log.last().unwrap(), "stop");
        r.pause(true);
        assert_eq!(
            r.local.log.last().unwrap(),
            "stop",
            "nothing active after stop"
        );
    }

    #[test]
    fn a_failed_load_does_not_change_which_backend_is_active() {
        struct Fails;
        impl Backend for Fails {
            fn load(&mut self, _: &Source) -> Result<(), String> {
                Err("no".into())
            }
            fn pause(&mut self, _: bool) {}
            fn seek(&mut self, _: u32) {}
            fn set_volume(&mut self, _: u8) {}
            fn set_speed(&mut self, _: u16) {}
            fn stop(&mut self) {}
        }
        let mut r = Router::new(Rec::default(), Fails);
        r.load(&Source::File("/m/a.mp4".into())).unwrap();
        assert!(r
            .load(&Source::YouTube {
                id: "dQw4w9WgXcQ".into(),
                kind: YtKind::Video
            })
            .is_err());
        r.pause(true);
        assert_eq!(
            r.local.log.last().unwrap(),
            "pause true",
            "still the local backend"
        );
    }

    /// The page, played by the test: it polls for a command and, for a load, reports what the
    /// real page would: loaded, then ended (or an error for the ids in `bad`).
    fn run_page_until_done(
        b: &Bridge,
        p: &mut Player<YouTubeBackend>,
        bad: &[&str],
    ) -> Vec<String> {
        let mut loaded = Vec::new();
        for _ in 0..60 {
            if matches!(p.state(), State::Ended | State::Failed(_)) {
                break;
            }
            let cmd = get(b, &format!("/t/{TOKEN}/cmd?wait=1")).body;
            let j = parse_json(&cmd).unwrap();
            if j.get("op").and_then(Json::as_str) == Some("load") {
                let id = j.get("id").and_then(Json::as_str).unwrap().to_string();
                if bad.contains(&id.as_str()) {
                    assert_eq!(post(b, "{\"event\":\"error\",\"code\":101}").status, 200);
                } else {
                    assert_eq!(
                        post(b, "{\"event\":\"loaded\",\"duration\":20}").status,
                        200
                    );
                    assert_eq!(post(b, "{\"event\":\"ended\"}").status, 200);
                }
                loaded.push(id);
            }
            for ev in b.drain_events() {
                p.on_event(ev);
            }
        }
        loaded
    }

    fn short(id: &str) -> Item {
        Item::new(Source::YouTube {
            id: id.into(),
            kind: YtKind::Short,
        })
    }

    #[test]
    fn shorts_play_one_after_another_through_real_sockets_without_any_input() {
        let b = bridge();
        let mut p = Player::new(b.backend());
        let ids = ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc", "ddddddddddd"];
        for id in ids {
            p.queue_mut().push(short(id));
        }
        p.command(Command::ShortsMode(true));
        p.command(Command::Play);
        let loaded = run_page_until_done(&b, &mut p, &[]);
        assert_eq!(loaded, ids, "every Short, in order, with no key pressed");
        assert_eq!(*p.state(), State::Ended);
    }

    #[test]
    fn a_short_youtube_will_not_play_is_skipped_and_the_feed_goes_on() {
        let b = bridge();
        let mut p = Player::new(b.backend());
        for id in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"] {
            p.queue_mut().push(short(id));
        }
        p.command(Command::ShortsMode(true));
        p.command(Command::Play);
        let loaded = run_page_until_done(&b, &mut p, &["bbbbbbbbbbb"]);
        assert_eq!(loaded, ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]);
        assert_eq!(*p.state(), State::Ended);
    }
}
