//! Shared kernel: ids, money and quantity arithmetic, injected clock, hash-chained audit log, idempotency, permissions, numbering, the ledger hand-off trait and JSON helpers.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

/// Uniform error type: a message a clerk can act on (names the field and the rule).
pub type Res<T> = Result<T, String>;

/// Convert any displayable error into a string.
pub fn e<E: std::fmt::Display>(x: E) -> String {
    x.to_string()
}

/// Generate a random v4 UUID id.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Caller context: which tenant, which user, which roles, optional company scope.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// Tenant identifier; every query in the system is filtered by it.
    pub tenant: String,
    /// User identifier for audit and notifications.
    pub user: String,
    /// Joined role names; each role maps to permission strings in role_permissions.
    pub roles: Vec<String>,
    /// Optional company scope (multi-company books).
    pub company: Option<String>,
}

impl Ctx {
    /// Build a context.
    pub fn new(tenant: &str, user: &str, roles: &[&str], company: Option<&str>) -> Self {
        Ctx {
            tenant: tenant.to_string(),
            user: user.to_string(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
            company: company.map(|s| s.to_string()),
        }
    }
    /// Company scope resolved with a fallback used for numbering series.
    pub fn company_or_default(&self) -> String {
        self.company
            .clone()
            .unwrap_or_else(|| "DEFAULT".to_string())
    }
}

// ---------- injected time ----------

/// Injected clock. Business logic never reads the system clock directly.
pub trait Clock {
    /// Unix seconds.
    fn now(&self) -> i64;
}

/// Real clock (only for main/serve; tests use FakeClock).
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Deterministic clock for tests.
pub struct FakeClock {
    t: Mutex<i64>,
}
impl FakeClock {
    /// Fixed starting instant.
    pub fn new(t: i64) -> Self {
        FakeClock { t: Mutex::new(t) }
    }
    /// Set the instant.
    pub fn set(&self, t: i64) {
        if let Ok(mut g) = self.t.lock() {
            *g = t;
        }
    }
    /// Advance by seconds.
    pub fn advance(&self, secs: i64) {
        if let Ok(mut g) = self.t.lock() {
            *g = g.saturating_add(secs);
        }
    }
}
impl Clock for FakeClock {
    fn now(&self) -> i64 {
        self.t.lock().map(|g| *g).unwrap_or(0)
    }
}

/// Civil date-time string (UTC) from unix seconds, without external crates.
pub fn ymd(now: i64) -> String {
    let days = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
    let (h, mi, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    // Howard Hinnant's civil_from_days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Calendar year (for numbering series) extracted from ymd(); 1970 on the impossible.
pub fn year_of(now: i64) -> i64 {
    ymd(now)
        .get(0..4)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1970)
}

// ---------- money and quantity (checked integer arithmetic) ----------

/// Checked money addition.
pub fn madd(a: i64, b: i64) -> Res<i64> {
    a.checked_add(b)
        .ok_or_else(|| "money overflow in addition".to_string())
}
/// Checked money subtraction.
pub fn msub(a: i64, b: i64) -> Res<i64> {
    a.checked_sub(b)
        .ok_or_else(|| "money overflow in subtraction".to_string())
}
/// Multiply money by an integer factor (exact).
pub fn mmul_i64(a: i64, n: i64) -> Res<i64> {
    a.checked_mul(n)
        .ok_or_else(|| "money overflow in multiplication".to_string())
}
/// Multiply money by a quantity in thousandths: money * qty_kth / 1000, rounded half away from zero.
pub fn mmul_qty(money: i64, qty_kth: i64) -> Res<i64> {
    let v = (money as i128) * (qty_kth as i128);
    let q = if v >= 0 {
        (v + 500) / 1000
    } else {
        (v - 500) / 1000
    };
    i64::try_from(q).map_err(|_| "money overflow multiplying by quantity".to_string())
}
/// Checked quantity addition (thousandths).
pub fn qadd(a: i64, b: i64) -> Res<i64> {
    a.checked_add(b)
        .ok_or_else(|| "quantity overflow in addition".to_string())
}
/// Checked quantity subtraction (thousandths).
pub fn qsub(a: i64, b: i64) -> Res<i64> {
    a.checked_sub(b)
        .ok_or_else(|| "quantity overflow in subtraction".to_string())
}
/// Multiply two thousandth-quantities: a*b/1000.
pub fn qmul(a: i64, b: i64) -> Res<i64> {
    let v = (a as i128) * (b as i128);
    let q = if v >= 0 {
        (v + 500) / 1000
    } else {
        (v - 500) / 1000
    };
    i64::try_from(q).map_err(|_| "quantity overflow in multiplication".to_string())
}
/// Multiply a thousandth-quantity by an integer factor.
pub fn qmul_i64(a: i64, n: i64) -> Res<i64> {
    a.checked_mul(n)
        .ok_or_else(|| "quantity overflow in multiplication".to_string())
}
/// Whole units to thousandths.
pub fn qty_from_int(n: i64) -> Res<i64> {
    n.checked_mul(1000)
        .ok_or_else(|| "quantity overflow".to_string())
}
/// Thousandths formatted for reports.
pub fn qty_str(q: i64) -> String {
    format!("{}.{:03}", q.div_euclid(1000), q.rem_euclid(1000).abs())
}
/// Money formatted for reports (currency handled by caller).
pub fn money_str(minor: i64) -> String {
    format!(
        "{}.{:02}",
        minor.div_euclid(100),
        minor.rem_euclid(100).abs()
    )
}

// ---------- hashing / canonical JSON ----------

/// Lowercase hex sha256 of a string.
pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Canonical JSON: serde_json's Map is a BTreeMap by default, so keys are sorted and
/// separators are minimal; two structurally equal values produce identical bytes.
pub fn canonical(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

// ---------- database helpers ----------

/// Open (and create) a SQLite database with foreign keys enforced.
pub fn open_db(path: &str) -> Res<Connection> {
    let c = Connection::open(path).map_err(e)?;
    c.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
        .map_err(e)?;
    Ok(c)
}

/// Open an in-memory database (tests).
pub fn open_memory() -> Res<Connection> {
    let c = Connection::open_in_memory().map_err(e)?;
    c.execute_batch("PRAGMA foreign_keys = ON;").map_err(e)?;
    Ok(c)
}

/// Run f inside a single immediate transaction. Inner helpers must NOT call in_tx
/// (no nesting); public operations wrap their whole state change in one call.
pub fn in_tx<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Res<T>) -> Res<T> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(e)?;
    match f(conn) {
        Ok(v) => {
            conn.execute_batch("COMMIT").map_err(e)?;
            Ok(v)
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

// ---------- hash-chained audit log ----------

/// Genesis previous_hash for the first audit row of a tenant.
pub const GENESIS: &str = "GENESIS";

fn audit_body(
    event_type: &str,
    entity_id: &str,
    payload: &Value,
    previous_hash: &str,
    created_at: i64,
) -> Value {
    json!({
        "event_type": event_type,
        "entity_id": entity_id,
        "payload": payload,
        "previous_hash": previous_hash,
        "created_at": created_at,
    })
}

/// Append one audit row. Must be called inside the same transaction as the state change.
/// Returns the assigned seq.
pub fn audit(
    conn: &Connection,
    tenant: &str,
    event_type: &str,
    entity_id: &str,
    payload: &Value,
    created_at: i64,
) -> Res<i64> {
    let last: Option<(i64, String)> = conn
        .query_row(
            "SELECT seq, entry_hash FROM audit_log WHERE tenant_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![tenant],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(e)?;
    let (seq, prev) = match last {
        Some((s, h)) => (s + 1, h),
        None => (1, GENESIS.to_string()),
    };
    let body = audit_body(event_type, entity_id, payload, &prev, created_at);
    let entry_hash = sha256_hex(&canonical(&body));
    conn.execute(
        "INSERT INTO audit_log(tenant_id, seq, event_type, entity_id, payload, previous_hash, entry_hash, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![tenant, seq, event_type, entity_id, canonical(payload), prev, entry_hash, created_at],
    ).map_err(e)?;
    Ok(seq)
}

/// Recompute the whole chain; on the first mismatch return an error naming the seq.
/// Returns the number of rows verified.
pub fn verify_audit(conn: &Connection, tenant: &str) -> Res<i64> {
    let mut stmt = conn
        .prepare(
            "SELECT seq, event_type, entity_id, payload, previous_hash, entry_hash, created_at \
         FROM audit_log WHERE tenant_id = ?1 ORDER BY seq ASC",
        )
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut prev = GENESIS.to_string();
    let mut expect: i64 = 1;
    for (seq, et, eid, payload_s, ph, eh, cat) in rows {
        if seq != expect {
            return Err(format!(
                "audit_log seq {seq} is out of order (expected {expect})"
            ));
        }
        if ph != prev {
            return Err(format!(
                "audit_log seq {seq} has previous_hash {ph} but expected {prev}"
            ));
        }
        let payload: Value = serde_json::from_str(&payload_s)
            .map_err(|x| format!("audit_log seq {seq} payload is not valid JSON: {x}"))?;
        let body = audit_body(&et, &eid, &payload, &ph, cat);
        if sha256_hex(&canonical(&body)) != eh {
            return Err(format!(
                "audit_log seq {seq} entry_hash mismatch: this row was altered after the fact"
            ));
        }
        prev = eh;
        expect += 1;
    }
    Ok(expect - 1)
}

// ---------- idempotency ----------

/// Look up a stored idempotent result. Same key + same payload -> stored result.
/// Same key + different payload -> error.
pub fn idem_check(
    conn: &Connection,
    tenant: &str,
    key: &str,
    payload: &Value,
) -> Res<Option<Value>> {
    let h = sha256_hex(&canonical(payload));
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT payload_hash, result_json FROM idempotency WHERE tenant_id = ?1 AND key = ?2",
            params![tenant, key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(e)?;
    match row {
        None => Ok(None),
        Some((ph, rj)) => {
            if ph != h {
                return Err(format!(
                    "idempotency key '{key}' was already used with a different payload"
                ));
            }
            serde_json::from_str(&rj)
                .map(Some)
                .map_err(|x| format!("stored idempotent result unreadable: {x}"))
        }
    }
}

/// Store an idempotent result.
pub fn idem_store(
    conn: &Connection,
    tenant: &str,
    key: &str,
    payload: &Value,
    result: &Value,
    now: i64,
) -> Res<()> {
    let h = sha256_hex(&canonical(payload));
    conn.execute(
        "INSERT INTO idempotency(tenant_id, key, payload_hash, result_json, created_at) VALUES (?1,?2,?3,?4,?5)",
        params![tenant, key, h, canonical(result), now],
    ).map_err(e)?;
    Ok(())
}

/// Execute f with idempotency semantics when a key is supplied.
pub fn idempotent<F: FnOnce(&Connection) -> Res<Value>>(
    conn: &Connection,
    tenant: &str,
    key: Option<&str>,
    payload: &Value,
    now: i64,
    f: F,
) -> Res<Value> {
    if let Some(k) = key {
        if let Some(v) = idem_check(conn, tenant, k, payload)? {
            return Ok(v);
        }
        let v = f(conn)?;
        idem_store(conn, tenant, k, payload, &v, now)?;
        Ok(v)
    } else {
        f(conn)
    }
}

// ---------- permissions (checked on every public operation) ----------

/// Require that at least one of the ctx roles holds `perm` (or the wildcard '*') in this tenant.
pub fn require_perm(conn: &Connection, ctx: &Ctx, perm: &str) -> Res<()> {
    let roles = serde_json::to_string(&ctx.roles).map_err(e)?;
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM role_permissions \
         WHERE tenant_id = ?1 AND role IN (SELECT value FROM json_each(?2)) \
           AND (permission = ?3 OR permission = '*')",
            params![ctx.tenant, roles, perm],
            |r| r.get(0),
        )
        .map_err(e)?;
    if n == 0 {
        return Err(format!(
            "permission denied: '{perm}' required (user '{}', roles '{}')",
            ctx.user,
            ctx.roles.join("+")
        ));
    }
    Ok(())
}

/// Maker-checker: the acting user may approve `amount_minor` only if one of their roles
/// carries an approval_limits row with limit -1 (unlimited) or >= amount. Otherwise error
/// naming the rule; callers route the document to escalation.
pub fn check_approval(conn: &Connection, ctx: &Ctx, amount_minor: i64) -> Res<()> {
    if amount_minor < 0 {
        return Err("amount must not be negative".to_string());
    }
    let roles = serde_json::to_string(&ctx.roles).map_err(e)?;
    let mut stmt = conn.prepare(
        "SELECT limit_minor FROM approval_limits WHERE tenant_id = ?1 AND role IN (SELECT value FROM json_each(?2))").map_err(e)?;
    let limits = stmt
        .query_map(params![ctx.tenant, roles], |r| r.get::<_, i64>(0))
        .map_err(e)?
        .collect::<std::result::Result<Vec<i64>, _>>()
        .map_err(e)?;
    if limits.is_empty() {
        return Err(format!(
            "permission denied: no approval limit configured for roles '{}'",
            ctx.roles.join("+")
        ));
    }
    if limits.iter().any(|l| *l == -1 || *l >= amount_minor) {
        return Ok(());
    }
    Err(format!(
        "amount {} exceeds the approval limit for roles '{}'; escalation required",
        money_str(amount_minor),
        ctx.roles.join("+")
    ))
}

// ---------- numbering series ----------

/// Next document number for (tenant, company, series, year), e.g. SO-ACME-2026-000042.
pub fn next_number(
    conn: &Connection,
    tenant: &str,
    company: &str,
    series: &str,
    now: i64,
) -> Res<String> {
    let year = year_of(now);
    conn.execute(
        "INSERT INTO numbering_series(tenant_id, company, series, year, next_seq) \
         VALUES (?1,?2,?3,?4,1) ON CONFLICT(tenant_id, company, series, year) DO NOTHING",
        params![tenant, company, series, year],
    )
    .map_err(e)?;
    let n: i64 = conn.query_row(
        "UPDATE numbering_series SET next_seq = next_seq + 1 \
         WHERE tenant_id = ?1 AND company = ?2 AND series = ?3 AND year = ?4 RETURNING next_seq - 1",
        params![tenant, company, series, year], |r| r.get(0),
    ).map_err(e)?;
    Ok(format!("{series}-{company}-{year}-{n:06}"))
}

// ---------- attachments (metadata only) and notifications ----------

/// Store attachment metadata (hash + size, never bytes). Returns the attachment id.
pub fn add_attachment(
    conn: &Connection,
    ctx: &Ctx,
    entity_type: &str,
    entity_id: &str,
    name: &str,
    hash: &str,
    size_bytes: i64,
    now: i64,
) -> Res<String> {
    require_perm(conn, ctx, "attachments.add")?;
    if name.trim().is_empty() {
        return Err("attachment name must not be empty".to_string());
    }
    if hash.trim().is_empty() {
        return Err("attachment hash must not be empty (sha256 hex expected)".to_string());
    }
    if size_bytes < 0 {
        return Err("attachment size_bytes must not be negative".to_string());
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO attachments(tenant_id, id, entity_type, entity_id, name, hash, size_bytes, created_by, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![ctx.tenant, id, entity_type, entity_id, name, hash, size_bytes, ctx.user, now],
    ).map_err(e)?;
    Ok(id)
}

/// Enqueue a notification. Returns the notification id.
pub fn notify(
    conn: &Connection,
    tenant: &str,
    user: &str,
    kind: &str,
    payload: &Value,
    now: i64,
) -> Res<String> {
    let id = new_id();
    conn.execute(
        "INSERT INTO notifications(tenant_id, id, user_name, kind, payload, read_flag, created_at) VALUES (?1,?2,?3,?4,?5,0,?6)",
        params![tenant, id, user, kind, canonical(payload), now],
    ).map_err(e)?;
    Ok(id)
}

// ---------- ledger hand-off (accounting lives behind this trait) ----------

/// One journal line. Debits and credits in minor units.
#[derive(Clone, Debug)]
pub struct JournalLine {
    /// Account code (chart of accounts owned by the ledger).
    pub account: String,
    /// Debit in minor units (>= 0).
    pub debit: i64,
    /// Credit in minor units (>= 0).
    pub credit: i64,
}

/// A balanced journal entry to post.
#[derive(Clone, Debug)]
pub struct JournalEntry {
    /// Posting date, "YYYY-MM-DD HH:MM:SS" UTC.
    pub date: String,
    /// Human memo (source document).
    pub memo: String,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Lines; sum(debits) must equal sum(credits).
    pub lines: Vec<JournalLine>,
}

/// Accounting hand-off. Implemented by the real ledger and by TestLedger in tests.
pub trait Ledger {
    /// Post an entry; returns the ledger's entry id.
    fn post(&self, tenant: &str, entry: &JournalEntry) -> Result<String, String>;
}

/// Validate an entry is non-empty, internally consistent and balanced.
pub fn validate_journal(entry: &JournalEntry) -> Res<()> {
    if entry.lines.is_empty() {
        return Err("journal entry has no lines".to_string());
    }
    let mut d: i128 = 0;
    let mut c: i128 = 0;
    for l in &entry.lines {
        if l.debit < 0 || l.credit < 0 {
            return Err(format!("account {} has a negative line amount", l.account));
        }
        if l.account.trim().is_empty() {
            return Err("journal line with empty account".to_string());
        }
        d += l.debit as i128;
        c += l.credit as i128;
    }
    if d != c {
        return Err(format!(
            "journal entry does not balance: debits {} != credits {} ({})",
            money_str(i64::try_from(d).unwrap_or(i64::MAX)),
            money_str(i64::try_from(c).unwrap_or(i64::MAX)),
            entry.memo
        ));
    }
    Ok(())
}

/// In-memory ledger for tests and the demo; records entries for assertion.
pub struct TestLedger {
    /// Recorded entries, in post order.
    pub entries: Mutex<Vec<JournalEntry>>,
}
impl TestLedger {
    /// Empty ledger.
    pub fn new() -> Self {
        TestLedger {
            entries: Mutex::new(Vec::new()),
        }
    }
    /// Clone of all recorded entries.
    pub fn recorded(&self) -> Vec<JournalEntry> {
        self.entries.lock().map(|g| g.clone()).unwrap_or_default()
    }
    /// Remove and return all recorded entries.
    pub fn take(&self) -> Vec<JournalEntry> {
        self.entries
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}
impl Default for TestLedger {
    fn default() -> Self {
        Self::new()
    }
}
impl Ledger for TestLedger {
    fn post(&self, _tenant: &str, entry: &JournalEntry) -> Result<String, String> {
        validate_journal(entry)?;
        if let Ok(mut g) = self.entries.lock() {
            g.push(entry.clone());
        }
        Ok(new_id())
    }
}

// ---------- JSON field helpers for the API layer ----------

/// Required string field.
pub fn jstr(v: &Value, key: &str) -> Res<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| format!("field '{key}' is required and must be a string"))
}
/// Optional string field.
pub fn jopt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(|s| s.to_string())
}
/// Required integer field (JSON numbers; no floats).
pub fn jint(v: &Value, key: &str) -> Res<i64> {
    v.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("field '{key}' is required and must be an integer"))
}
/// Optional integer field.
pub fn jopt_int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}
/// Optional bool field (default false).
pub fn jbool(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        // Query strings arrive as text: `?unread_only=true`.
        Some(Value::String(s)) => matches!(s.as_str(), "true" | "1" | "yes"),
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}
/// Required array field.
pub fn jarr<'a>(v: &'a Value, key: &str) -> Res<&'a Vec<Value>> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("field '{key}' is required and must be an array"))
}

/// Sum one i64 column from a query, tenant-scoped; NULL yields 0.
pub fn scalar_i64(conn: &Connection, sql: &str, tenant: &str) -> Res<i64> {
    // SQLite COALESCE(...,0) keeps this total; callers pass SQL with one ?1 tenant parameter.
    conn.query_row(sql, params![tenant], |r| r.get::<_, i64>(0))
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(0),
            other => Err(e(other)),
        })
}
