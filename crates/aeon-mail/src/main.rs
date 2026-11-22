//! AEON-MAIL: AI-native mail server (single-binary).
//!
//! Modes: `serve` | `migrate` | `create-user <email> <password>`.
//!
//! Status: unverified. Known gaps before real use:
//! - no TLS (STARTTLS / implicit TLS) yet, so AUTH is plain text: bind to
//!   localhost only until TLS lands;
//! - SMTP requires AUTH for every message (submission only); receiving mail
//!   from other servers (MX role) is not implemented;
//! - the outbound `delivery_queue` is filled but no relay worker sends it;
//! - IMAP is a minimal subset (single-message FETCH, `\Seen` STORE, UNSEEN SEARCH).
#![forbid(unsafe_code)]

use aeon_web::html::{tokenize, Token};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tracing::{info, warn};

// ============================================================
// Section 1: Configuration
// ============================================================
#[derive(Clone)]
struct Config {
    db_path: PathBuf,
    smtp_addr: String,
    imap_addr: String,
    http_addr: String,
    /// e.g. `http://127.0.0.1:11434/v1` (any OpenAI-compatible server). Unset = AI disabled.
    ai_endpoint: Option<String>,
    /// `None` = local/anonymous.
    ai_api_key: Option<String>,
    ai_model: String,
    max_message_bytes: usize,
    max_attachment_bytes: usize,
    /// Attempts per window per peer address.
    login_rate_limit: u32,
    rate_window_secs: u64,
}

impl Config {
    fn from_env() -> Self {
        Self {
            db_path: std::env::var("AEON_DB").map_or_else(|_| "aeon.db".into(), PathBuf::from),
            smtp_addr: std::env::var("AEON_SMTP").unwrap_or_else(|_| "127.0.0.1:2525".into()),
            imap_addr: std::env::var("AEON_IMAP").unwrap_or_else(|_| "127.0.0.1:1143".into()),
            http_addr: std::env::var("AEON_HTTP").unwrap_or_else(|_| "127.0.0.1:8580".into()),
            ai_endpoint: std::env::var("AEON_AI_ENDPOINT").ok(),
            ai_api_key: std::env::var("AEON_AI_KEY").ok(),
            ai_model: std::env::var("AEON_AI_MODEL").unwrap_or_else(|_| "llama3.1".into()),
            max_message_bytes: 25 * 1024 * 1024,
            max_attachment_bytes: 10 * 1024 * 1024,
            login_rate_limit: 5,
            rate_window_secs: 300,
        }
    }
}

// ============================================================
// Section 2: Database schema and migrations
// ============================================================
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS users(
        id INTEGER PRIMARY KEY,
        email TEXT NOT NULL UNIQUE,
        password_hash TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')));",
    "CREATE TABLE IF NOT EXISTS sessions(
        token TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at TEXT NOT NULL DEFAULT (datetime('now')));",
    "CREATE TABLE IF NOT EXISTS mailboxes(
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        uid_validity INTEGER NOT NULL DEFAULT 1,
        UNIQUE(user_id, name));",
    "CREATE TABLE IF NOT EXISTS messages(
        id INTEGER PRIMARY KEY,
        mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
        uid INTEGER NOT NULL,
        message_id TEXT,
        from_addr TEXT,
        to_addr TEXT,
        subject TEXT,
        date TEXT,
        raw BLOB NOT NULL,
        raw_sha256 TEXT NOT NULL,
        flags TEXT NOT NULL DEFAULT '',
        internal_date TEXT NOT NULL DEFAULT (datetime('now')),
        UNIQUE(mailbox_id, uid),
        UNIQUE(mailbox_id, raw_sha256));",
    "CREATE INDEX IF NOT EXISTS idx_messages_subject ON messages(subject);",
    "CREATE INDEX IF NOT EXISTS idx_messages_from ON messages(from_addr);",
    "CREATE TABLE IF NOT EXISTS attachments(
        id INTEGER PRIMARY KEY,
        message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        filename TEXT,
        mime_type TEXT,
        size INTEGER NOT NULL,
        content BLOB NOT NULL);",
    "CREATE TABLE IF NOT EXISTS delivery_queue(
        id INTEGER PRIMARY KEY,
        recipient TEXT NOT NULL,
        raw BLOB NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_attempt TEXT NOT NULL DEFAULT (datetime('now')),
        last_error TEXT);",
    "CREATE TABLE IF NOT EXISTS agents(
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        token_sha256 TEXT NOT NULL,
        revoked INTEGER NOT NULL DEFAULT 0);",
    "CREATE TABLE IF NOT EXISTS agent_permissions(
        agent_id INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        mailbox TEXT NOT NULL,
        permission TEXT NOT NULL CHECK(permission IN ('read','write','draft','send')),
        PRIMARY KEY(agent_id, mailbox, permission));",
    "CREATE TABLE IF NOT EXISTS audit_log(
        id INTEGER PRIMARY KEY,
        actor TEXT NOT NULL,
        action TEXT NOT NULL,
        detail TEXT,
        created_at TEXT NOT NULL DEFAULT (datetime('now')));",
    "CREATE TABLE IF NOT EXISTS ai_summaries(
        message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
        summary TEXT NOT NULL,
        model TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')));",
];

fn open_db(path: &PathBuf) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for m in MIGRATIONS {
        conn.execute_batch(m)?;
    }
    Ok(())
}

// ============================================================
// Section 3: Password hashing (argon2id)
// ============================================================
fn hash_password(pw: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

// ============================================================
// Section 4: Rate limiter (login throttling, shared across connections)
// ============================================================
struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
    limit: u32,
    window: Duration,
}

impl RateLimiter {
    fn new(limit: u32, window_secs: u64) -> Self {
        Self {
            hits: Mutex::new(HashMap::new()),
            limit,
            window: Duration::from_secs(window_secs),
        }
    }

    fn check(&self, key: &str) -> bool {
        let Ok(mut m) = self.hits.lock() else {
            return false;
        };
        let now = Instant::now();
        let v = m.entry(key.to_string()).or_default();
        v.retain(|t| now.duration_since(*t) < self.window);
        if v.len() >= self.limit as usize {
            return false;
        }
        v.push(now);
        true
    }
}

/// Rate-limit key for a peer: the IP without the port.
fn peer_key(stream: &TcpStream) -> String {
    stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default()
}

// ============================================================
// Section 5: HTML sanitization
// ============================================================
/// Elements whose whole content is dropped.
fn dropped_element(tag: &str) -> bool {
    matches!(
        tag,
        "script"
            | "style"
            | "iframe"
            | "frame"
            | "frameset"
            | "object"
            | "embed"
            | "applet"
            | "template"
            | "noscript"
    )
}

/// Void elements that are removed outright.
fn removed_void(tag: &str) -> bool {
    matches!(tag, "link" | "meta" | "base")
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

fn safe_url(v: &str) -> bool {
    let compact: String = v
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    !(compact.starts_with("javascript:")
        || compact.starts_with("vbscript:")
        || compact.starts_with("data:"))
}

/// Rebuild HTML keeping only passive markup: active elements, event
/// handlers, inline styles and script/data URLs are removed. Text is
/// re-escaped, so malformed markup cannot smuggle tags through.
fn sanitize_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut skip: Option<String> = None;
    for t in tokenize(input) {
        if let Some(open) = &skip {
            if matches!(&t, Token::End(tag) if tag == open) {
                skip = None;
            }
            continue;
        }
        match t {
            Token::Text(s) => escape_text(&s, &mut out),
            Token::Start {
                tag,
                attrs,
                self_closing,
            } => {
                if dropped_element(&tag) {
                    if !self_closing {
                        skip = Some(tag);
                    }
                    continue;
                }
                if removed_void(&tag) {
                    continue;
                }
                out.push('<');
                out.push_str(&tag);
                for (name, value) in attrs {
                    let unsafe_attr = name.starts_with("on")
                        || name == "style"
                        || name == "srcdoc"
                        || (matches!(
                            name.as_str(),
                            "href" | "src" | "action" | "formaction" | "xlink:href"
                        ) && !safe_url(&value));
                    if unsafe_attr
                        || !name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':')
                    {
                        continue;
                    }
                    out.push(' ');
                    out.push_str(&name);
                    out.push_str("=\"");
                    escape_text(&value, &mut out);
                    out.push('"');
                }
                out.push_str(if self_closing { "/>" } else { ">" });
            }
            Token::End(tag) => {
                if !dropped_element(&tag) && !removed_void(&tag) {
                    out.push_str("</");
                    out.push_str(&tag);
                    out.push('>');
                }
            }
        }
    }
    out
}

// ============================================================
// Section 6: MIME parsing and message storage
// ============================================================
struct ParsedMessage {
    message_id: Option<String>,
    from: Option<String>,
    to: Option<String>,
    subject: Option<String>,
    date: Option<String>,
    /// (filename, mime type, bytes)
    attachments: Vec<(String, String, Vec<u8>)>,
}

fn parse_message(raw: &[u8]) -> ParsedMessage {
    let mut pm = ParsedMessage {
        message_id: None,
        from: None,
        to: None,
        subject: None,
        date: None,
        attachments: Vec::new(),
    };
    if let Ok(parsed) = mailparse::parse_mail(raw) {
        for h in &parsed.headers {
            let value = h.get_value();
            let value = value.trim();
            match h.get_key().to_ascii_lowercase().as_str() {
                "message-id" => pm.message_id = Some(value.trim_matches(['<', '>']).to_string()),
                "from" => pm.from = Some(value.to_string()),
                "to" => pm.to = Some(value.to_string()),
                "subject" => pm.subject = Some(value.to_string()),
                "date" => pm.date = Some(value.to_string()),
                _ => {}
            }
        }
        collect_attachments(&parsed, &mut pm.attachments);
    }
    pm
}

fn collect_attachments(part: &mailparse::ParsedMail, out: &mut Vec<(String, String, Vec<u8>)>) {
    let disp = part.get_content_disposition();
    if disp.disposition == mailparse::DispositionType::Attachment {
        if let Ok(body) = part.get_body_raw() {
            let name = disp
                .params
                .get("filename")
                .cloned()
                .unwrap_or_else(|| "unnamed".into());
            out.push((name, part.ctype.mimetype.clone(), body));
        }
    }
    for sub in &part.subparts {
        collect_attachments(sub, out);
    }
}

/// Store a message (deduplicated by SHA-256 per mailbox). Returns the row id
/// and how many attachments were skipped for size.
fn store_message(
    conn: &Connection,
    mailbox_id: i64,
    raw: &[u8],
    max_att: usize,
) -> rusqlite::Result<(i64, usize)> {
    let parsed = parse_message(raw);
    let sha = hex::encode(Sha256::digest(raw));
    if let Ok(existing) = conn.query_row(
        "SELECT id FROM messages WHERE mailbox_id=?1 AND raw_sha256=?2",
        params![mailbox_id, sha],
        |r| r.get::<_, i64>(0),
    ) {
        return Ok((existing, 0));
    }
    let uid: i64 = conn.query_row(
        "SELECT COALESCE(MAX(uid),0)+1 FROM messages WHERE mailbox_id=?1",
        params![mailbox_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO messages(mailbox_id,uid,message_id,from_addr,to_addr,subject,date,raw,raw_sha256)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![mailbox_id, uid, parsed.message_id, parsed.from, parsed.to, parsed.subject, parsed.date, raw, sha],
    )?;
    let msg_id = conn.last_insert_rowid();
    let mut skipped = 0;
    for (name, mime, bytes) in parsed.attachments {
        if bytes.len() > max_att {
            skipped += 1;
            continue;
        }
        conn.execute(
            "INSERT INTO attachments(message_id,filename,mime_type,size,content) VALUES(?1,?2,?3,?4,?5)",
            params![msg_id, name, mime, bytes.len() as i64, bytes],
        )?;
    }
    Ok((msg_id, skipped))
}

// ============================================================
// Section 7: Users, sessions, agents, audit
// ============================================================
fn create_user(conn: &Connection, email: &str, password: &str) -> Result<(), String> {
    if !email.contains('@') || password.chars().count() < 12 {
        return Err("email invalid or password < 12 chars".into());
    }
    let hash = hash_password(password)?;
    conn.execute(
        "INSERT INTO users(email,password_hash) VALUES(?1,?2)",
        params![email, hash],
    )
    .map_err(|e| e.to_string())?;
    let uid = conn.last_insert_rowid();
    for mbox in ["INBOX", "Sent", "Drafts", "Archive", "Spam", "Trash"] {
        conn.execute(
            "INSERT OR IGNORE INTO mailboxes(user_id,name) VALUES(?1,?2)",
            params![uid, mbox],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn authenticate(conn: &Connection, email: &str, password: &str) -> Option<i64> {
    let (id, hash): (i64, String) = conn
        .query_row(
            "SELECT id,password_hash FROM users WHERE email=?1",
            params![email],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    verify_password(password, &hash).then_some(id)
}

fn create_session(conn: &Connection, user_id: i64) -> rusqlite::Result<String> {
    let token = hex::encode(rand::random::<[u8; 32]>());
    conn.execute(
        "INSERT INTO sessions(token,user_id) VALUES(?1,?2)",
        params![token, user_id],
    )?;
    Ok(token)
}

fn user_for_token(conn: &Connection, token: &str) -> Option<i64> {
    conn.query_row(
        "SELECT user_id FROM sessions WHERE token=?1",
        params![token],
        |r| r.get(0),
    )
    .ok()
}

fn agent_has_perm(conn: &Connection, agent_id: i64, mailbox: &str, perm: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM agent_permissions ap JOIN agents a ON a.id=ap.agent_id
         WHERE ap.agent_id=?1 AND ap.mailbox=?2 AND ap.permission=?3 AND a.revoked=0",
        params![agent_id, mailbox, perm],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n > 0)
}

fn audit(conn: &Connection, actor: &str, action: &str, detail: &str) {
    if let Err(e) = conn.execute(
        "INSERT INTO audit_log(actor,action,detail) VALUES(?1,?2,?3)",
        params![actor, action, detail],
    ) {
        warn!("audit write failed: {e}");
    }
}

// ============================================================
// Section 8: AI provider (optional, provider-independent)
// ============================================================
#[async_trait::async_trait]
trait AiProvider: Send + Sync {
    async fn summarize(&self, text: &str) -> Result<String, String>;
}

struct DisabledAi;

#[async_trait::async_trait]
impl AiProvider for DisabledAi {
    async fn summarize(&self, _t: &str) -> Result<String, String> {
        Err("AI disabled (set AEON_AI_ENDPOINT)".into())
    }
}

struct OpenAiCompatibleAi {
    endpoint: String,
    key: Option<String>,
    model: String,
}

#[async_trait::async_trait]
impl AiProvider for OpenAiCompatibleAi {
    async fn summarize(&self, text: &str) -> Result<String, String> {
        // Never ship whole mailboxes blindly.
        let truncated: String = text.chars().take(12_000).collect();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client
            .post(format!(
                "{}/chat/completions",
                self.endpoint.trim_end_matches('/')
            ))
            .json(&serde_json::json!({
                "model": self.model,
                "messages": [
                    {"role": "system", "content": "Summarize the email. Do not invent facts."},
                    {"role": "user", "content": truncated}
                ]
            }));
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "malformed AI response".into())
    }
}

// ============================================================
// Section 9: SMTP server (authenticated submission; no STARTTLS yet)
// ============================================================
fn send_line(w: &mut TcpStream, s: &str) {
    if let Err(e) = w.write_all(format!("{s}\r\n").as_bytes()) {
        warn!("write failed: {e}");
    }
}

/// Address inside `MAIL FROM:<...>` / `RCPT TO:<...>`.
fn path_arg(line: &str) -> String {
    let after = line.split_once(':').map_or("", |(_, rest)| rest);
    let inner = after.trim();
    let inner = inner.split_whitespace().next().unwrap_or("");
    inner.trim_matches(|c| c == '<' || c == '>').to_string()
}

fn handle_smtp(stream: TcpStream, cfg: Arc<Config>, limiter: Arc<RateLimiter>) {
    let peer = peer_key(&stream);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    send_line(&mut writer, "220 aeon-mail SMTP ready");

    let mut authed_user: Option<String> = None;
    let mut mail_from: Option<String> = None;
    let mut rcpts: Vec<String> = Vec::new();

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            break;
        }
        let line_trim = line.trim_end().to_string();
        let upper = line_trim.to_ascii_uppercase();
        let cmd = upper.split_whitespace().next().unwrap_or("");

        match cmd {
            "EHLO" | "HELO" => {
                send_line(
                    &mut writer,
                    "250-aeon-mail\r\n250-AUTH PLAIN\r\n250-8BITMIME\r\n250 SIZE 26214400",
                );
            }
            "AUTH" => {
                if !limiter.check(&peer) {
                    send_line(&mut writer, "454 too many attempts, try later");
                    continue;
                }
                // AUTH PLAIN <base64(authzid \0 user \0 pass)>
                let parts: Vec<&str> = line_trim.split_whitespace().collect();
                if parts.get(1).map(|m| m.to_ascii_uppercase()) != Some("PLAIN".into()) {
                    send_line(&mut writer, "504 only AUTH PLAIN is supported");
                    continue;
                }
                let decoded = parts
                    .get(2)
                    .and_then(|b| B64.decode(b).ok())
                    .and_then(|d| String::from_utf8(d).ok());
                let Some(d) = decoded else {
                    send_line(&mut writer, "501 malformed AUTH");
                    continue;
                };
                let segs: Vec<&str> = d.split('\0').collect();
                let (user, pass) = (
                    segs.get(1).copied().unwrap_or(""),
                    segs.get(2).copied().unwrap_or(""),
                );
                let Ok(conn) = open_db(&cfg.db_path) else {
                    send_line(&mut writer, "451 db error");
                    continue;
                };
                if authenticate(&conn, user, pass).is_some() {
                    authed_user = Some(user.to_string());
                    audit(&conn, user, "smtp_auth", &peer);
                    send_line(&mut writer, "235 authenticated");
                } else {
                    audit(&conn, user, "smtp_auth_failed", &peer);
                    send_line(&mut writer, "535 authentication failed");
                }
            }
            "MAIL" => {
                let Some(user) = &authed_user else {
                    send_line(&mut writer, "530 authentication required");
                    continue;
                };
                let from = path_arg(&line_trim);
                if !from.eq_ignore_ascii_case(user) {
                    send_line(&mut writer, "553 sender must match the authenticated user");
                    continue;
                }
                mail_from = Some(from);
                rcpts.clear();
                send_line(&mut writer, "250 OK");
            }
            "RCPT" => {
                if mail_from.is_none() {
                    send_line(&mut writer, "503 MAIL first");
                    continue;
                }
                let to = path_arg(&line_trim);
                if !to.contains('@') {
                    send_line(&mut writer, "501 bad recipient");
                    continue;
                }
                rcpts.push(to);
                send_line(&mut writer, "250 OK");
            }
            "DATA" => {
                if rcpts.is_empty() {
                    send_line(&mut writer, "503 no recipients");
                    continue;
                }
                send_line(&mut writer, "354 end with <CR><LF>.<CR><LF>");
                let mut raw: Vec<u8> = Vec::new();
                let mut oversize = false;
                loop {
                    let mut l: Vec<u8> = Vec::new();
                    match reader.read_until(b'\n', &mut l) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if l == b".\r\n" || l == b".\n" {
                        break;
                    }
                    let unstuffed: &[u8] = if l.starts_with(b"..") {
                        l.get(1..).unwrap_or_default()
                    } else {
                        &l
                    };
                    if !oversize {
                        raw.extend_from_slice(unstuffed);
                        if raw.len() > cfg.max_message_bytes {
                            warn!("oversize message from {peer}");
                            oversize = true;
                        }
                    }
                }
                if oversize {
                    send_line(&mut writer, "552 message exceeds size limit");
                    mail_from = None;
                    rcpts.clear();
                    continue;
                }
                let Ok(conn) = open_db(&cfg.db_path) else {
                    send_line(&mut writer, "451 db error");
                    continue;
                };
                // Local recipients get the message in INBOX; others are queued.
                for addr in &rcpts {
                    let local: Option<i64> = conn
                        .query_row(
                            "SELECT m.id FROM users u JOIN mailboxes m ON m.user_id=u.id WHERE u.email=?1 AND m.name='INBOX'",
                            params![addr],
                            |r| r.get(0),
                        )
                        .ok();
                    let result = match local {
                        Some(mbox) => store_message(&conn, mbox, &raw, cfg.max_attachment_bytes)
                            .map(|(_, skipped)| {
                                if skipped > 0 {
                                    warn!("{skipped} oversized attachment(s) skipped for {addr}");
                                }
                            }),
                        None => conn
                            .execute(
                                "INSERT INTO delivery_queue(recipient,raw) VALUES(?1,?2)",
                                params![addr, raw],
                            )
                            .map(|_| ()),
                    };
                    if let Err(e) = result {
                        warn!("delivery to {addr} failed: {e}");
                    }
                }
                audit(
                    &conn,
                    authed_user.as_deref().unwrap_or("smtp"),
                    "message_submitted",
                    &format!("{} rcpt(s) from {peer}", rcpts.len()),
                );
                send_line(&mut writer, "250 queued");
                mail_from = None;
                rcpts.clear();
            }
            "RSET" => {
                mail_from = None;
                rcpts.clear();
                send_line(&mut writer, "250 OK");
            }
            "NOOP" => send_line(&mut writer, "250 OK"),
            "QUIT" => {
                send_line(&mut writer, "221 bye");
                break;
            }
            _ => send_line(&mut writer, "502 command not implemented"),
        }
    }
}

// ============================================================
// Section 10: IMAP server (minimal subset)
// ============================================================
fn unquote(s: &str) -> &str {
    s.trim_matches('"')
}

fn handle_imap(stream: TcpStream, cfg: Arc<Config>, limiter: Arc<RateLimiter>) {
    let peer = peer_key(&stream);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    send_line(&mut writer, "* OK aeon-mail IMAP4rev1 ready");

    let mut user: Option<i64> = None;
    let mut selected_mbox: Option<i64> = None;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            break;
        }
        let parts: Vec<&str> = line.trim_end().splitn(3, ' ').collect();
        if parts.len() < 2 {
            send_line(&mut writer, "* BAD empty command");
            continue;
        }
        let tag = parts[0].to_string();
        let cmd = parts[1].to_ascii_uppercase();
        let rest = parts.get(2).copied().unwrap_or("");
        let Ok(conn) = open_db(&cfg.db_path) else {
            send_line(&mut writer, &format!("{tag} NO database unavailable"));
            continue;
        };

        match cmd.as_str() {
            "CAPABILITY" => send_line(
                &mut writer,
                &format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done"),
            ),
            "LOGIN" => {
                if !limiter.check(&peer) {
                    send_line(
                        &mut writer,
                        &format!("{tag} NO too many attempts, try later"),
                    );
                    continue;
                }
                let args: Vec<&str> = rest.split_whitespace().collect();
                let (u, p) = (
                    unquote(args.first().copied().unwrap_or("")),
                    unquote(args.get(1).copied().unwrap_or("")),
                );
                if let Some(uid) = authenticate(&conn, u, p) {
                    user = Some(uid);
                    audit(&conn, u, "imap_login", &peer);
                    send_line(&mut writer, &format!("{tag} OK logged in"));
                } else {
                    audit(&conn, u, "imap_login_failed", &peer);
                    send_line(&mut writer, &format!("{tag} NO invalid credentials"));
                }
            }
            "LIST" => {
                let Some(uid) = user else {
                    send_line(&mut writer, &format!("{tag} NO login first"));
                    continue;
                };
                let names: Vec<String> = conn
                    .prepare("SELECT name FROM mailboxes WHERE user_id=?1 ORDER BY name")
                    .and_then(|mut stmt| {
                        stmt.query_map(params![uid], |r| r.get(0))
                            .map(|rows| rows.flatten().collect())
                    })
                    .unwrap_or_default();
                for n in names {
                    send_line(
                        &mut writer,
                        &format!("* LIST (\\HasNoChildren) \"/\" \"{n}\""),
                    );
                }
                send_line(&mut writer, &format!("{tag} OK LIST done"));
            }
            "SELECT" => {
                let Some(uid) = user else {
                    send_line(&mut writer, &format!("{tag} NO login first"));
                    continue;
                };
                let name = unquote(rest);
                let row: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM mailboxes WHERE user_id=?1 AND name=?2",
                        params![uid, name],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(id) = row {
                    selected_mbox = Some(id);
                    let n: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM messages WHERE mailbox_id=?1",
                            params![id],
                            |r| r.get(0),
                        )
                        .unwrap_or(0);
                    send_line(
                        &mut writer,
                        &format!("* {n} EXISTS\r\n{tag} OK [READ-WRITE] selected"),
                    );
                } else {
                    send_line(&mut writer, &format!("{tag} NO mailbox not found"));
                }
            }
            "FETCH" => {
                let (Some(_), Some(mbox)) = (user, selected_mbox) else {
                    send_line(&mut writer, &format!("{tag} NO no mailbox selected"));
                    continue;
                };
                let seq: i64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1)
                    .max(1);
                let row: Option<(i64, Vec<u8>)> = conn
                    .query_row(
                        "SELECT uid, raw FROM messages WHERE mailbox_id=?1 ORDER BY uid LIMIT 1 OFFSET ?2",
                        params![mbox, seq - 1],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                if let Some((uidn, raw)) = row {
                    send_line(
                        &mut writer,
                        &format!("* {seq} FETCH (UID {uidn} RFC822 {{{}}}", raw.len()),
                    );
                    let _ = writer.write_all(&raw);
                    let _ = writer.write_all(b")\r\n");
                    send_line(&mut writer, &format!("{tag} OK FETCH done"));
                } else {
                    send_line(&mut writer, &format!("{tag} NO no such message"));
                }
            }
            "STORE" => {
                let (Some(_), Some(mbox)) = (user, selected_mbox) else {
                    send_line(&mut writer, &format!("{tag} NO no mailbox selected"));
                    continue;
                };
                // STORE <seq> +FLAGS (\Seen): only \Seen is supported.
                let seq: i64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1)
                    .max(1);
                let _ = conn.execute(
                    "UPDATE messages SET flags = flags || ' \\Seen'
                     WHERE mailbox_id=?1 AND flags NOT LIKE '%\\Seen%'
                       AND uid=(SELECT uid FROM messages WHERE mailbox_id=?1 ORDER BY uid LIMIT 1 OFFSET ?2)",
                    params![mbox, seq - 1],
                );
                send_line(&mut writer, &format!("{tag} OK STORE done"));
            }
            "SEARCH" => {
                let (Some(_), Some(mbox)) = (user, selected_mbox) else {
                    send_line(&mut writer, &format!("{tag} NO no mailbox selected"));
                    continue;
                };
                let query = if rest.to_ascii_uppercase().starts_with("UNSEEN") {
                    "SELECT uid FROM messages WHERE mailbox_id=?1 AND flags NOT LIKE '%\\Seen%' ORDER BY uid"
                } else {
                    "SELECT uid FROM messages WHERE mailbox_id=?1 ORDER BY uid"
                };
                let uids: Vec<String> = conn
                    .prepare(query)
                    .and_then(|mut stmt| {
                        stmt.query_map(params![mbox], |r| r.get::<_, i64>(0))
                            .map(|rows| rows.flatten().map(|u| u.to_string()).collect())
                    })
                    .unwrap_or_default();
                send_line(
                    &mut writer,
                    &format!("* SEARCH {}\r\n{tag} OK SEARCH done", uids.join(" ")),
                );
            }
            "NOOP" => send_line(&mut writer, &format!("{tag} OK NOOP done")),
            "LOGOUT" => {
                send_line(
                    &mut writer,
                    &format!("* BYE aeon-mail logging out\r\n{tag} OK LOGOUT done"),
                );
                break;
            }
            _ => send_line(&mut writer, &format!("{tag} BAD unsupported command")),
        }
    }
}

// ============================================================
// Section 11: HTTP API
// ============================================================
#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    ai: Arc<dyn AiProvider>,
    limiter: Arc<RateLimiter>,
}

#[derive(serde::Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

/// Exchange email and password for a session token (rate-limited).
async fn api_login(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::Json(req): axum::Json<LoginRequest>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    use axum::http::StatusCode;
    let ip = addr.ip().to_string();
    if !st.limiter.check(&ip) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "too many attempts".into()));
    }
    let conn =
        open_db(&st.cfg.db_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(uid) = authenticate(&conn, &req.email, &req.password) else {
        audit(&conn, &req.email, "http_login_failed", &ip);
        return Err((StatusCode::UNAUTHORIZED, "invalid credentials".into()));
    };
    let token = create_session(&conn, uid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    audit(&conn, &req.email, "http_login", &ip);
    Ok(axum::Json(serde_json::json!({"token": token})))
}

fn find_part<'a>(
    p: &'a mailparse::ParsedMail<'a>,
    mime: &str,
) -> Option<&'a mailparse::ParsedMail<'a>> {
    if p.ctype.mimetype.eq_ignore_ascii_case(mime)
        && p.get_content_disposition().disposition != mailparse::DispositionType::Attachment
    {
        return Some(p);
    }
    p.subparts.iter().find_map(|s| find_part(s, mime))
}

/// Best displayable body: sanitized HTML part, else escaped plain text.
fn display_body(raw: &[u8]) -> String {
    let Ok(parsed) = mailparse::parse_mail(raw) else {
        return String::new();
    };
    if let Some(html) = find_part(&parsed, "text/html").and_then(|p| p.get_body().ok()) {
        return sanitize_html(&html);
    }
    let text = find_part(&parsed, "text/plain")
        .and_then(|p| p.get_body().ok())
        .unwrap_or_default();
    let mut out = String::new();
    escape_text(&text, &mut out);
    out
}

/// One of the caller's messages with a safe body.
async fn api_message(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let (conn, uid) = bearer_user(&st, &headers)?;
    let row: Option<(Option<String>, Option<String>, Vec<u8>)> = conn
        .query_row(
            "SELECT m.subject, m.from_addr, m.raw FROM messages m JOIN mailboxes b ON b.id=m.mailbox_id
             WHERE m.id=?1 AND b.user_id=?2",
            params![id, uid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (subject, from, raw) = row.ok_or((
        axum::http::StatusCode::NOT_FOUND,
        "no such message".to_string(),
    ))?;
    Ok(axum::Json(serde_json::json!({
        "id": id, "subject": subject, "from": from, "body_html": display_body(&raw)
    })))
}

#[derive(serde::Deserialize)]
struct MailboxQuery {
    mailbox: String,
}

/// Agent view: subjects in a mailbox, only with an unrevoked `read` grant.
/// Agents authenticate with `X-Agent-Name` and `X-Agent-Token`.
async fn api_agent_messages(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::extract::Query(q): axum::extract::Query<MailboxQuery>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    use axum::http::StatusCode;
    let header = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let (name, token) = (header("x-agent-name"), header("x-agent-token"));
    let conn =
        open_db(&st.cfg.db_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let digest = hex::encode(Sha256::digest(token.as_bytes()));
    let agent: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, owner_user_id FROM agents WHERE name=?1 AND token_sha256=?2 AND revoked=0",
            params![name, digest],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let (aid, owner) = agent.ok_or((
        StatusCode::UNAUTHORIZED,
        "unknown or revoked agent".to_string(),
    ))?;
    if !agent_has_perm(&conn, aid, &q.mailbox, "read") {
        audit(&conn, &format!("agent:{name}"), "read_denied", &q.mailbox);
        return Err((
            StatusCode::FORBIDDEN,
            "no read permission for this mailbox".into(),
        ));
    }
    let subjects: Vec<String> = conn
        .prepare(
            "SELECT COALESCE(m.subject,'') FROM messages m JOIN mailboxes b ON b.id=m.mailbox_id
             WHERE b.user_id=?1 AND b.name=?2 ORDER BY m.uid DESC LIMIT 100",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![owner, q.mailbox], |r| r.get(0))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    audit(&conn, &format!("agent:{name}"), "read", &q.mailbox);
    Ok(axum::Json(
        serde_json::json!({"mailbox": q.mailbox, "subjects": subjects}),
    ))
}

type ApiError = (axum::http::StatusCode, String);

#[derive(Serialize)]
struct Health {
    status: &'static str,
    db: bool,
}

async fn http_health(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> axum::Json<Health> {
    let db = open_db(&st.cfg.db_path).is_ok_and(|c| migrate(&c).is_ok());
    axum::Json(Health {
        status: if db { "ok" } else { "degraded" },
        db,
    })
}

fn bearer_user(
    st: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(Connection, i64), ApiError> {
    use axum::http::StatusCode;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;
    let conn =
        open_db(&st.cfg.db_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let uid = user_for_token(&conn, token)
        .ok_or((StatusCode::UNAUTHORIZED, "invalid token".to_string()))?;
    Ok((conn, uid))
}

async fn api_summary(
    axum::extract::State(st): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let (conn, uid) = bearer_user(&st, &headers)?;
    let msgs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages m JOIN mailboxes b ON b.id=m.mailbox_id WHERE b.user_id=?1",
            params![uid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(axum::Json(
        serde_json::json!({"user_id": uid, "message_count": msgs}),
    ))
}

/// Summarise the request body. Only invoked by an explicit user request
/// with a user session token; agents cannot call it.
async fn api_ai_summarize(
    axum::extract::State(st): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let uid = {
        let (conn, uid) = bearer_user(&st, &headers)?;
        audit(
            &conn,
            &format!("user:{uid}"),
            "ai_summarize_requested",
            "explicit user request",
        );
        uid
    };
    let summary = st
        .ai
        .summarize(&body)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_GATEWAY, e))?;
    Ok(axum::Json(
        serde_json::json!({"user_id": uid, "summary": summary}),
    ))
}

// ============================================================
// Section 12: Entrypoint and modes
// ============================================================
fn spawn_listener(addr: String, kind: &'static str, cfg: Arc<Config>, limiter: Arc<RateLimiter>) {
    std::thread::spawn(move || {
        let listener = match std::net::TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                warn!("{kind}: cannot bind {addr}: {e}");
                return;
            }
        };
        info!("{kind} listening on {addr}");
        for stream in listener.incoming().flatten() {
            let (c, l) = (cfg.clone(), limiter.clone());
            std::thread::spawn(move || {
                if kind == "smtp" {
                    handle_smtp(stream, c, l);
                } else {
                    handle_imap(stream, c, l);
                }
            });
        }
    });
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cfg = Arc::new(Config::from_env());
    let args: Vec<String> = std::env::args().collect();

    let conn = match open_db(&cfg.db_path).and_then(|c| migrate(&c).map(|()| c)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("database error: {e}");
            std::process::exit(1);
        }
    };

    match args.get(1).map(String::as_str) {
        Some("migrate") => println!("migrations applied"),
        Some("create-user") => {
            let (Some(email), Some(pw)) = (args.get(2), args.get(3)) else {
                eprintln!("usage: aeon-mail create-user <email> <password>");
                std::process::exit(2);
            };
            match create_user(&conn, email, pw) {
                Ok(()) => println!("user {email} created"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("serve") | None => {
            drop(conn);
            let limiter = Arc::new(RateLimiter::new(cfg.login_rate_limit, cfg.rate_window_secs));
            spawn_listener(cfg.smtp_addr.clone(), "smtp", cfg.clone(), limiter.clone());
            spawn_listener(cfg.imap_addr.clone(), "imap", cfg.clone(), limiter.clone());

            let ai: Arc<dyn AiProvider> = match &cfg.ai_endpoint {
                Some(ep) => Arc::new(OpenAiCompatibleAi {
                    endpoint: ep.clone(),
                    key: cfg.ai_api_key.clone(),
                    model: cfg.ai_model.clone(),
                }),
                None => Arc::new(DisabledAi),
            };
            info!(
                "AI provider: {}",
                if cfg.ai_endpoint.is_some() {
                    "openai-compatible"
                } else {
                    "disabled"
                }
            );

            let state = AppState {
                cfg: cfg.clone(),
                ai,
                limiter,
            };
            let app = axum::Router::new()
                .route("/health", axum::routing::get(http_health))
                .route("/healthz", axum::routing::get(|| async { "ok" }))
                .route("/api/v1/login", axum::routing::post(api_login))
                .route("/api/v1/summary", axum::routing::get(api_summary))
                .route("/api/v1/messages/:id", axum::routing::get(api_message))
                .route(
                    "/api/v1/agent/messages",
                    axum::routing::get(api_agent_messages),
                )
                .route(
                    "/api/v1/ai/summarize",
                    axum::routing::post(api_ai_summarize),
                )
                .with_state(state)
                .layer(axum::extract::DefaultBodyLimit::max(cfg.max_message_bytes));

            let listener = match tokio::net::TcpListener::bind(&cfg.http_addr).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("cannot bind http {}: {e}", cfg.http_addr);
                    std::process::exit(1);
                }
            };
            info!("http listening on {}", cfg.http_addr);
            let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("http server error: {e}");
                std::process::exit(1);
            }
        }
        Some(other) => {
            eprintln!("unknown mode: {other}. modes: serve | migrate | create-user");
            std::process::exit(2);
        }
    }
}

// ============================================================
// Section 13: Tests
// ============================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not-a-hash"));
    }

    #[test]
    fn sanitize_strips_active_content_and_keeps_text() {
        let dirty = r#"<p onclick="evil()">Meet on Monday</p><script>alert(1)</script><b>ok</b><a href="javascript:x()">l</a><img src=x onerror=y()>"#;
        let clean = sanitize_html(dirty);
        let lower = clean.to_ascii_lowercase();
        assert!(!lower.contains("<script"));
        assert!(!lower.contains("alert"));
        assert!(!lower.contains("onclick") && !lower.contains("onerror"));
        assert!(!lower.contains("javascript:"));
        assert!(clean.contains("Meet on Monday"));
        assert!(clean.contains("<b>ok</b>"));
        assert!(clean.contains("<img src=\"x\">"));
        // Non-ASCII input must not panic.
        assert_eq!(sanitize_html("héllo <i>wörld</i>"), "héllo <i>wörld</i>");
    }

    #[test]
    fn rate_limiter_blocks_after_limit() {
        let rl = RateLimiter::new(3, 60);
        assert!(rl.check("1.2.3.4"));
        assert!(rl.check("1.2.3.4"));
        assert!(rl.check("1.2.3.4"));
        assert!(!rl.check("1.2.3.4"));
        assert!(rl.check("5.6.7.8"));
    }

    #[test]
    fn migration_creates_tables() {
        let conn = temp_db();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 10, "10 tables (the other 2 statements are indexes)");
        migrate(&conn).unwrap();
    }

    #[test]
    fn user_and_session_flow() {
        let conn = temp_db();
        create_user(&conn, "a@b.c", "twelvecharsmin").unwrap();
        assert!(create_user(&conn, "a@b.c", "twelvecharsmin").is_err());
        assert!(create_user(&conn, "short@b.c", "short").is_err());
        let uid = authenticate(&conn, "a@b.c", "twelvecharsmin").expect("auth");
        let t = create_session(&conn, uid).unwrap();
        assert_eq!(user_for_token(&conn, &t), Some(uid));
        assert!(authenticate(&conn, "a@b.c", "wrong").is_none());
        assert!(user_for_token(&conn, "nope").is_none());
    }

    #[test]
    fn message_store_dedup_and_parse() {
        let conn = temp_db();
        create_user(&conn, "m@x.y", "twelvecharsmin").unwrap();
        let mbox: i64 = conn
            .query_row("SELECT id FROM mailboxes WHERE name='INBOX'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let raw = b"Message-ID: <abc@test>\r\nFrom: s@x.y\r\nTo: m@x.y\r\nSubject: Hi\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nContent-Type: text/plain\r\n\r\nBody text\r\n";
        let (id1, _) = store_message(&conn, mbox, raw, 1024).unwrap();
        let (id2, _) = store_message(&conn, mbox, raw, 1024).unwrap();
        assert_eq!(id1, id2, "dedup by sha256");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE mailbox_id=?1",
                params![mbox],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let (subj, mid): (String, String) = conn
            .query_row(
                "SELECT subject, message_id FROM messages WHERE id=?1",
                params![id1],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(subj, "Hi");
        assert_eq!(mid, "abc@test");
    }

    #[test]
    fn agent_permission_enforced() {
        let conn = temp_db();
        create_user(&conn, "o@x.y", "twelvecharsmin").unwrap();
        let uid: i64 = conn
            .query_row("SELECT id FROM users WHERE email='o@x.y'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO agents(name,owner_user_id,token_sha256) VALUES('bot',?1,'deadbeef')",
            params![uid],
        )
        .unwrap();
        let aid: i64 = conn
            .query_row("SELECT id FROM agents WHERE name='bot'", [], |r| r.get(0))
            .unwrap();
        assert!(!agent_has_perm(&conn, aid, "INBOX", "read"));
        conn.execute(
            "INSERT INTO agent_permissions(agent_id,mailbox,permission) VALUES(?1,'INBOX','read')",
            params![aid],
        )
        .unwrap();
        assert!(agent_has_perm(&conn, aid, "INBOX", "read"));
        assert!(
            !agent_has_perm(&conn, aid, "INBOX", "send"),
            "no privilege creep"
        );
        conn.execute("UPDATE agents SET revoked=1 WHERE id=?1", params![aid])
            .unwrap();
        assert!(
            !agent_has_perm(&conn, aid, "INBOX", "read"),
            "revocation works"
        );
    }

    #[test]
    fn mime_attachment_extracted_and_size_limited() {
        let raw = b"Content-Type: multipart/mixed; boundary=xyz\r\nMessage-ID: <m@t>\r\nSubject: att\r\n\r\n--xyz\r\nContent-Type: text/plain\r\n\r\nbody\r\n--xyz\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"a.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nQUJD\r\n--xyz--\r\n";
        let p = parse_message(raw);
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].0, "a.bin");
        assert_eq!(p.attachments[0].2, b"ABC");

        let conn = temp_db();
        create_user(&conn, "z@x.y", "twelvecharsmin").unwrap();
        let mbox: i64 = conn
            .query_row("SELECT id FROM mailboxes WHERE name='INBOX'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let (_, skipped) = store_message(&conn, mbox, raw, 2).unwrap();
        assert_eq!(skipped, 1);
    }

    #[test]
    fn display_body_prefers_safe_html() {
        let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain <x>\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Hi</p><script>bad()</script>\r\n--b--\r\n";
        let body = display_body(raw);
        assert!(body.contains("<p>Hi</p>"));
        assert!(!body.contains("bad()"));
        let plain = display_body(b"Content-Type: text/plain\r\n\r\na <b> c\r\n");
        assert!(plain.contains("a &lt;b&gt; c"));
    }

    #[test]
    fn smtp_path_argument() {
        assert_eq!(path_arg("MAIL FROM:<a@b.c> SIZE=100"), "a@b.c");
        assert_eq!(path_arg("RCPT TO: <x@y.z>"), "x@y.z");
        assert_eq!(path_arg("RCPT"), "");
    }
}
