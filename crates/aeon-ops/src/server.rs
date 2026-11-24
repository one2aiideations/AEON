//! A small HTTP/1.1 server over `std::net` that exposes the router.
//!
//! Identity comes from the `X-Tenant`, `X-User`, `X-Roles` and `X-Company` headers and is NOT
//! authenticated: keep the server on loopback or put an authenticating gateway in front of it.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::api::{self, Api};
use crate::common::{self, Clock, Ctx, JournalEntry, Ledger, SystemClock};

/// Where and how to serve.
pub struct ServerConfig {
    /// Address and port to bind, for example `127.0.0.1:8787`.
    pub bind: String,
    /// SQLite database file.
    pub db_path: String,
    /// Largest accepted request body in bytes.
    pub max_body: usize,
    /// Seconds to wait for the client while reading a request.
    pub read_timeout_secs: u64,
}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// Method, for example `GET`.
    pub method: String,
    /// Request target including any query string.
    pub target: String,
    /// Headers in the order received.
    pub headers: Vec<(String, String)>,
    /// Body bytes (exactly `Content-Length` of them).
    pub body: Vec<u8>,
}

/// A response to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// JSON body bytes.
    pub body: Vec<u8>,
}

/// Why a request could not be parsed (yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// More bytes are needed.
    Incomplete,
    /// The request is malformed.
    BadRequest(String),
    /// The body is larger than allowed.
    TooLarge,
}

const MAX_HEAD: usize = 16 * 1024;

/// Parses the bytes read so far into a request.
pub fn parse_request(bytes: &[u8], max_body: usize) -> Result<HttpRequest, HttpError> {
    let head_end = bytes.windows(4).position(|w| w == b"\r\n\r\n");
    let Some(head_end) = head_end else {
        if bytes.len() > MAX_HEAD {
            return Err(HttpError::BadRequest(
                "header block exceeds 16 KiB".to_string(),
            ));
        }
        return Err(HttpError::Incomplete);
    };
    if head_end > MAX_HEAD {
        return Err(HttpError::BadRequest(
            "header block exceeds 16 KiB".to_string(),
        ));
    }
    let head = std::str::from_utf8(bytes.get(..head_end).unwrap_or_default())
        .map_err(|_| HttpError::BadRequest("headers are not valid UTF-8".to_string()))?;
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| HttpError::BadRequest("empty request line".to_string()))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| HttpError::BadRequest("missing method".to_string()))?;
    let target = parts
        .next()
        .ok_or_else(|| HttpError::BadRequest("missing target".to_string()))?;
    let version = parts
        .next()
        .ok_or_else(|| HttpError::BadRequest("missing HTTP version".to_string()))?;
    if parts.next().is_some() {
        return Err(HttpError::BadRequest(
            "too many tokens on request line".to_string(),
        ));
    }
    if !method
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(HttpError::BadRequest(format!(
            "invalid method '{}'",
            method
        )));
    }
    if !target.starts_with('/') || target.chars().any(|c| c.is_control() || c == ' ') {
        return Err(HttpError::BadRequest(
            "target must be an origin-form path".to_string(),
        ));
    }
    if version != "HTTP/1.1" {
        return Err(HttpError::BadRequest(format!(
            "unsupported version '{}'",
            version
        )));
    }

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| HttpError::BadRequest(format!("malformed header line '{}'", line)))?;
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(HttpError::BadRequest(format!(
                "invalid header name '{}'",
                name
            )));
        }
        let value = value.trim();
        if value.chars().any(|c| c.is_control()) {
            return Err(HttpError::BadRequest(format!(
                "invalid header value for '{}'",
                name
            )));
        }
        headers.push((name.to_string(), value.to_string()));
    }
    if headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("transfer-encoding"))
    {
        return Err(HttpError::BadRequest(
            "transfer encoding is not supported".to_string(),
        ));
    }
    let lengths: Vec<&str> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.as_str())
        .collect();
    if lengths.len() > 1 {
        return Err(HttpError::BadRequest(
            "multiple content-length headers".to_string(),
        ));
    }
    let content_length = match lengths.first() {
        Some(v) => Some(
            v.parse::<usize>()
                .map_err(|_| HttpError::BadRequest(format!("invalid content-length '{}'", v)))?,
        ),
        None => None,
    };
    let length = content_length.unwrap_or(0);
    if length > max_body {
        return Err(HttpError::TooLarge);
    }
    let body_start = head_end + 4;
    let available = bytes.len().saturating_sub(body_start);
    if content_length.is_none() && available > 0 {
        return Err(HttpError::BadRequest(
            "body without content-length".to_string(),
        ));
    }
    if available < length {
        return Err(HttpError::Incomplete);
    }
    let body = bytes
        .get(body_start..body_start + length)
        .unwrap_or_default()
        .to_vec();
    Ok(HttpRequest {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body,
    })
}

fn header_value(request: &HttpRequest, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn json_response(status: u16, value: &Value) -> HttpResponse {
    let body = serde_json::to_vec(value)
        .unwrap_or_else(|_| b"{\"error\":\"serialization failed\"}".to_vec());
    HttpResponse { status, body }
}

/// Handles one parsed request against an open database connection.
pub fn handle_request(
    conn: &Connection,
    ledger: &dyn Ledger,
    clock: &dyn Clock,
    request: &HttpRequest,
) -> HttpResponse {
    let path = request.target.split('?').next().unwrap_or("");
    let is_health = request.method == "GET" && path == "/health";
    let tenant = header_value(request, "x-tenant");
    let user = header_value(request, "x-user");
    if !is_health && (tenant.is_none() || user.is_none()) {
        return json_response(
            401,
            &json!({ "error": "missing identity: X-Tenant and X-User headers are required" }),
        );
    }
    let body_value = if request.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&request.body) {
            Ok(v) if v.is_object() => v,
            Ok(_) => return json_response(400, &json!({ "error": "body must be a JSON object" })),
            Err(e) => {
                return json_response(
                    400,
                    &json!({ "error": format!("invalid JSON body: {}", e) }),
                )
            }
        }
    };
    let roles = header_value(request, "x-roles")
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    let ctx = Ctx {
        tenant: tenant.unwrap_or_default(),
        user: user.unwrap_or_default(),
        roles,
        company: header_value(request, "x-company"),
    };
    let router = Api::new(conn, ledger, clock);
    match router.handle(&request.method, &request.target, &body_value, &ctx) {
        Ok(value) => json_response(200, &value),
        Err(message) => json_response(api::status_for(&message), &json!({ "error": message })),
    }
}

/// Renders a response with its headers.
pub fn render_response(r: &HttpResponse) -> Vec<u8> {
    let reason = match r.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        r.status,
        reason,
        r.body.len()
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(&r.body);
    out
}

/// An in-memory ledger that validates every entry it is given.
pub struct LoggingLedger {
    /// Entries posted so far.
    pub entries: Mutex<Vec<JournalEntry>>,
}

impl LoggingLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl Default for LoggingLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger for LoggingLedger {
    fn post(&self, _tenant: &str, entry: &JournalEntry) -> Result<String, String> {
        common::validate_journal(entry)?;
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| "ledger mutex poisoned".to_string())?;
        guard.push(entry.clone());
        Ok(common::new_id())
    }
}

fn split_bind(bind: &str) -> Result<(String, u16), String> {
    let bad = || format!("invalid bind address '{}'", bind);
    let (host, port) = if bind.starts_with('[') {
        let end = bind.find(']').ok_or_else(bad)?;
        let port = bind
            .get(end + 1..)
            .and_then(|r| r.strip_prefix(':'))
            .ok_or_else(bad)?;
        (bind.get(..=end).ok_or_else(bad)?, port)
    } else {
        let idx = bind.rfind(':').ok_or_else(bad)?;
        (
            bind.get(..idx).ok_or_else(bad)?,
            bind.get(idx + 1..).ok_or_else(bad)?,
        )
    };
    if host.is_empty() {
        return Err(bad());
    }
    let port: u16 = port
        .parse()
        .map_err(|_| format!("invalid port in bind address '{}'", bind))?;
    Ok((host.to_string(), port))
}

fn write_response(stream: &mut TcpStream, response: &HttpResponse) {
    let _ = stream.write_all(&render_response(response));
    let _ = stream.flush();
}

fn handle_connection(
    mut stream: TcpStream,
    db_path: &str,
    max_body: usize,
    read_timeout_secs: u64,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(read_timeout_secs)));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let request = loop {
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(_) => return,
        };
        buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
        match parse_request(&buf, max_body) {
            Ok(r) => break r,
            Err(HttpError::Incomplete) if n > 0 => {
                if buf.len() > MAX_HEAD + max_body + 4 {
                    write_response(
                        &mut stream,
                        &json_response(413, &json!({ "error": "payload too large" })),
                    );
                    return;
                }
            }
            Err(HttpError::Incomplete) => {
                if !buf.is_empty() {
                    write_response(
                        &mut stream,
                        &json_response(400, &json!({ "error": "incomplete request" })),
                    );
                }
                return;
            }
            Err(HttpError::TooLarge) => {
                write_response(
                    &mut stream,
                    &json_response(413, &json!({ "error": "payload too large" })),
                );
                return;
            }
            Err(HttpError::BadRequest(m)) => {
                write_response(&mut stream, &json_response(400, &json!({ "error": m })));
                return;
            }
        }
    };
    let conn = match common::open_db(db_path) {
        Ok(c) => c,
        Err(m) => {
            write_response(&mut stream, &json_response(500, &json!({ "error": m })));
            return;
        }
    };
    let _ = conn.busy_timeout(Duration::from_secs(30));
    let response = handle_request(&conn, &LoggingLedger::new(), &SystemClock, &request);
    write_response(&mut stream, &response);
}

/// Serves until the process ends. Refuses a non-loopback address unless
/// `AEON_OPS_ALLOW_REMOTE=1` is set.
pub fn serve(config: &ServerConfig) -> Result<(), String> {
    if config.max_body == 0 {
        return Err("max_body must be greater than zero".to_string());
    }
    let (host, _port) = split_bind(&config.bind)?;
    let loopback = host == "127.0.0.1" || host == "[::1]" || host.eq_ignore_ascii_case("localhost");
    let allow_remote = std::env::var("AEON_OPS_ALLOW_REMOTE").is_ok_and(|v| v == "1");
    if !loopback && !allow_remote {
        return Err(format!(
            "refusing to bind non-loopback address '{}'; set AEON_OPS_ALLOW_REMOTE=1 to allow it",
            config.bind
        ));
    }
    if !loopback {
        eprintln!(
            "warning: aeon-ops is binding to {}; the X-Tenant/X-User/X-Roles identity headers are NOT authenticated; put an authenticating gateway in front of this service",
            config.bind
        );
    }
    let listener =
        TcpListener::bind(&config.bind).map_err(|e| format!("bind {}: {}", config.bind, e))?;
    for stream in listener.incoming().flatten() {
        let db_path = config.db_path.clone();
        let (max_body, timeout) = (config.max_body, config.read_timeout_secs);
        std::thread::spawn(move || handle_connection(stream, &db_path, max_body, timeout));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin_reports;
    use crate::common::{new_id, JournalLine};

    fn full_request(method: &str, target: &str, extra: &[(&str, &str)], body: &str) -> Vec<u8> {
        let mut s = format!("{} {} HTTP/1.1\r\nHost: localhost\r\n", method, target);
        for (k, v) in extra {
            s.push_str(&format!("{}: {}\r\n", k, v));
        }
        if !body.is_empty() {
            s.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        s.push_str("\r\n");
        s.push_str(body);
        s.into_bytes()
    }

    fn temp_db() -> (String, Connection) {
        let path = std::env::temp_dir()
            .join(format!("aeon-ops-server-test-{}.db", new_id()))
            .to_string_lossy()
            .into_owned();
        let conn = common::open_db(&path).unwrap();
        crate::schema::create_schema(&conn).unwrap();
        admin_reports::bootstrap_tenant(&conn, "acme", "admin", 1).unwrap();
        (path, conn)
    }

    fn identity_headers() -> Vec<(String, String)> {
        vec![
            ("x-tenant".to_string(), "acme".to_string()),
            ("x-user".to_string(), "admin".to_string()),
            ("x-roles".to_string(), "admin".to_string()),
        ]
    }

    fn req(method: &str, target: &str, headers: Vec<(String, String)>, body: &str) -> HttpRequest {
        HttpRequest {
            method: method.to_string(),
            target: target.to_string(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    fn call(conn: &Connection, r: &HttpRequest) -> HttpResponse {
        handle_request(conn, &LoggingLedger::new(), &SystemClock, r)
    }

    #[test]
    fn parse_valid_get() {
        let r = parse_request(&full_request("GET", "/health", &[], ""), 1024).unwrap();
        assert_eq!((r.method.as_str(), r.target.as_str()), ("GET", "/health"));
        assert!(r.body.is_empty());
    }

    #[test]
    fn parse_valid_post_with_body() {
        let r = parse_request(&full_request("POST", "/things", &[], "{\"a\":1}"), 1024).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, b"{\"a\":1}");
    }

    #[test]
    fn parse_oversize_body() {
        assert_eq!(
            parse_request(&full_request("POST", "/things", &[], "0123456789"), 4),
            Err(HttpError::TooLarge)
        );
    }

    #[test]
    fn parse_body_without_content_length() {
        let bytes = b"POST /x HTTP/1.1\r\nHost: h\r\n\r\nhello".to_vec();
        match parse_request(&bytes, 1024) {
            Err(HttpError::BadRequest(m)) => assert!(m.contains("content-length")),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn parse_chunked_refused() {
        let bytes = full_request("POST", "/x", &[("Transfer-Encoding", "chunked")], "");
        match parse_request(&bytes, 1024) {
            Err(HttpError::BadRequest(m)) => assert!(m.contains("transfer")),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn parse_garbage_request_line() {
        assert!(matches!(
            parse_request(b"garbage\r\n\r\n", 1024),
            Err(HttpError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_partial_headers_incomplete() {
        assert_eq!(
            parse_request(b"GET / HTTP/1.1\r\nHost: loc", 1024),
            Err(HttpError::Incomplete)
        );
    }

    #[test]
    fn parse_huge_header_block() {
        let mut bytes = b"GET / HTTP/1.1\r\n".to_vec();
        let line = b"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n";
        while bytes.len() <= MAX_HEAD + line.len() {
            bytes.extend_from_slice(line);
        }
        assert!(matches!(
            parse_request(&bytes, 1024),
            Err(HttpError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_lowercase_header_names() {
        let r = parse_request(
            &full_request("GET", "/x", &[("x-tenant", "acme"), ("x-user", "bob")], ""),
            1024,
        )
        .unwrap();
        assert_eq!(header_value(&r, "X-TENANT"), Some("acme".to_string()));
        assert_eq!(header_value(&r, "x-user"), Some("bob".to_string()));
    }

    #[test]
    fn parse_content_length_incomplete_body() {
        let bytes = b"POST /x HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc".to_vec();
        assert_eq!(parse_request(&bytes, 1024), Err(HttpError::Incomplete));
    }

    #[test]
    fn parse_invalid_content_length() {
        let bytes = b"POST /x HTTP/1.1\r\nContent-Length: abc\r\n\r\n".to_vec();
        assert!(matches!(
            parse_request(&bytes, 1024),
            Err(HttpError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_multiple_content_length_refused() {
        let bytes = b"POST /x HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\na".to_vec();
        assert!(matches!(
            parse_request(&bytes, 1024),
            Err(HttpError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_http_1_0_refused() {
        assert!(matches!(
            parse_request(b"GET / HTTP/1.0\r\n\r\n", 1024),
            Err(HttpError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_empty_request_incomplete() {
        assert_eq!(parse_request(&[], 1024), Err(HttpError::Incomplete));
    }

    #[test]
    fn parse_keeps_the_query_string() {
        let r = parse_request(b"GET /a?b=c HTTP/1.1\r\nHost: h\r\n\r\n", 16).unwrap();
        assert_eq!(r.target, "/a?b=c");
    }

    #[test]
    fn parse_never_panics_on_arbitrary_prefixes() {
        let full = full_request("POST", "/x?y=1", &[("X-Tenant", "t")], "{\"k\":\"v\"}");
        for n in 0..full.len() {
            let _ = parse_request(&full[..n], 64);
        }
        for i in 0..full.len() {
            let mut m = full.clone();
            m[i] ^= 0xff;
            let _ = parse_request(&m, 64);
        }
    }

    #[test]
    fn handle_health_without_identity() {
        let (_p, conn) = temp_db();
        let resp = call(&conn, &req("GET", "/health", vec![], ""));
        assert_eq!(resp.status, 200);
        assert!(String::from_utf8_lossy(&resp.body).contains("\"ok\""));
    }

    #[test]
    fn handle_missing_identity_is_401() {
        let (_p, conn) = temp_db();
        assert_eq!(
            call(&conn, &req("GET", "/admin/warehouses", vec![], "")).status,
            401
        );
    }

    #[test]
    fn handle_missing_user_is_401() {
        let (_p, conn) = temp_db();
        let headers = vec![("x-tenant".to_string(), "acme".to_string())];
        assert_eq!(
            call(&conn, &req("GET", "/admin/warehouses", headers, "")).status,
            401
        );
    }

    #[test]
    fn handle_invalid_json_body_is_400() {
        let (_p, conn) = temp_db();
        assert_eq!(
            call(
                &conn,
                &req("POST", "/admin/warehouses", identity_headers(), "not json")
            )
            .status,
            400
        );
    }

    #[test]
    fn handle_array_body_is_400() {
        let (_p, conn) = temp_db();
        assert_eq!(
            call(
                &conn,
                &req("POST", "/admin/warehouses", identity_headers(), "[1]")
            )
            .status,
            400
        );
    }

    #[test]
    fn handle_unknown_route_is_404() {
        let (_p, conn) = temp_db();
        assert_eq!(
            call(&conn, &req("GET", "/no-such-route", identity_headers(), "")).status,
            404
        );
    }

    #[test]
    fn handle_wrong_method_is_405() {
        let (_p, conn) = temp_db();
        assert_eq!(
            call(&conn, &req("POST", "/health", identity_headers(), "")).status,
            405
        );
    }

    #[test]
    fn handle_user_without_permission_is_403() {
        let (_p, conn) = temp_db();
        let headers = vec![
            ("x-tenant".to_string(), "acme".to_string()),
            ("x-user".to_string(), "nobody".to_string()),
            ("x-roles".to_string(), "no-such-role".to_string()),
        ];
        let resp = call(
            &conn,
            &req(
                "POST",
                "/admin/warehouses",
                headers,
                "{\"code\":\"W9\",\"name\":\"Nope\"}",
            ),
        );
        assert_eq!(resp.status, 403);
    }

    #[test]
    fn handle_duplicate_is_409() {
        let (_p, conn) = temp_db();
        let body = "{\"code\":\"W1\",\"name\":\"Main\"}";
        assert_eq!(
            call(
                &conn,
                &req("POST", "/admin/warehouses", identity_headers(), body)
            )
            .status,
            200
        );
        assert_eq!(
            call(
                &conn,
                &req("POST", "/admin/warehouses", identity_headers(), body)
            )
            .status,
            409
        );
    }

    #[test]
    fn handle_create_then_read_round_trip() {
        let (_p, conn) = temp_db();
        let create = call(
            &conn,
            &req(
                "POST",
                "/admin/warehouses",
                identity_headers(),
                "{\"code\":\"W1\",\"name\":\"Main\"}",
            ),
        );
        assert_eq!(
            create.status,
            200,
            "create body: {}",
            String::from_utf8_lossy(&create.body)
        );
        let list = call(
            &conn,
            &req("GET", "/admin/warehouses", identity_headers(), ""),
        );
        assert_eq!(list.status, 200);
        assert!(String::from_utf8_lossy(&list.body).contains("W1"));
    }

    #[test]
    fn handle_query_string_reaches_the_router() {
        let (_p, conn) = temp_db();
        let r = call(
            &conn,
            &req(
                "GET",
                "/reports/payroll?period=2026-03",
                identity_headers(),
                "",
            ),
        );
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        assert!(String::from_utf8_lossy(&r.body).contains("2026-03"));
    }

    #[test]
    fn render_response_format() {
        let bytes = render_response(&HttpResponse {
            status: 200,
            body: b"{}".to_vec(),
        });
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("{}"));
    }

    fn entry(debit: i64, credit: i64) -> JournalEntry {
        JournalEntry {
            date: "2026-03-01".to_string(),
            memo: "m".to_string(),
            currency: "USD".to_string(),
            lines: vec![
                JournalLine {
                    account: "1000".to_string(),
                    debit,
                    credit: 0,
                },
                JournalLine {
                    account: "4000".to_string(),
                    debit: 0,
                    credit,
                },
            ],
        }
    }

    #[test]
    fn logging_ledger_rejects_unbalanced_entry() {
        assert!(LoggingLedger::new().post("acme", &entry(100, 50)).is_err());
    }

    #[test]
    fn logging_ledger_accepts_balanced_entry() {
        let ledger = LoggingLedger::new();
        assert!(ledger.post("acme", &entry(100, 100)).is_ok());
        assert_eq!(ledger.entries.lock().map(|g| g.len()).unwrap_or(0), 1);
    }

    #[test]
    fn bind_addresses_are_split_correctly() {
        assert_eq!(
            split_bind("127.0.0.1:8787"),
            Ok(("127.0.0.1".to_string(), 8787))
        );
        assert_eq!(split_bind("[::1]:80"), Ok(("[::1]".to_string(), 80)));
        for bad in ["nocolon", ":80", "host:", "host:99999", "[::1]80", "[::1"] {
            assert!(split_bind(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn serve_refuses_remote_bind() {
        let config = ServerConfig {
            bind: "203.0.113.10:9999".to_string(),
            db_path: ":memory:".to_string(),
            max_body: 1024,
            read_timeout_secs: 1,
        };
        // Skip when the operator has allowed remote binds in this environment.
        if std::env::var("AEON_OPS_ALLOW_REMOTE").is_ok_and(|v| v == "1") {
            return;
        }
        assert!(serve(&config).unwrap_err().contains("refusing"));
    }

    #[test]
    fn serve_rejects_invalid_port_and_zero_body() {
        let mut config = ServerConfig {
            bind: "127.0.0.1:not-a-port".to_string(),
            db_path: ":memory:".to_string(),
            max_body: 1024,
            read_timeout_secs: 1,
        };
        assert!(serve(&config).is_err());
        config.bind = "127.0.0.1:0".to_string();
        config.max_body = 0;
        assert!(serve(&config).is_err());
    }

    #[test]
    fn a_real_socket_round_trip_works() {
        let (path, _keep) = temp_db();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let p = path.clone();
        std::thread::spawn(move || {
            if let Ok((s, _)) = listener.accept() {
                handle_connection(s, &p, 1024 * 1024, 5);
            }
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(&full_request("GET", "/health", &[], ""))
            .unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
        assert!(out.contains("\"ok\""));
    }
}
