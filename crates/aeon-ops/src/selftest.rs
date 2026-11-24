//! Built-in smoke test behind `aeon-ops test`.
//!
//! This is a SMALL stand-in (a handful of checks). It is meant to be replaced by the full
//! 150-check suite; the flow tests in `tests/` cover the detail in the meantime.

use crate::common::{self, Ctx, FakeClock, TestLedger};
use crate::{admin_reports, api::Api, demo, schema};
use serde_json::{json, Value};

/// Runs the smoke checks against a fresh in-memory database.
pub fn run_selftest() -> Result<Value, String> {
    let mut checks = serde_json::Map::new();
    let mut pass = |name: &str, ok: bool, detail: String| -> Result<(), String> {
        if ok {
            checks.insert(name.to_string(), json!("PASS"));
            Ok(())
        } else {
            Err(format!("check '{name}' failed: {detail}"))
        }
    };

    let conn = common::open_memory()?;
    schema::create_schema(&conn)?;
    let ledger = TestLedger::new();
    let clock = FakeClock::new(1_795_000_000);
    let admin = Ctx::new("selftest", "admin", &["admin"], Some("ACME"));

    let boot = admin_reports::bootstrap_tenant(&conn, "selftest", "admin", 1);
    pass("bootstrap_creates_admin", boot.is_ok(), format!("{boot:?}"))?;
    let again = admin_reports::bootstrap_tenant(&conn, "selftest", "mallory", 2);
    pass("bootstrap_runs_once", again.is_err(), format!("{again:?}"))?;

    let api = Api::new(&conn, &ledger, &clock);
    let health = api.handle("GET", "/health", &json!({}), &admin)?;
    pass("health_ok", health["status"] == "ok", health.to_string())?;
    let nf = api.handle("GET", "/nope", &json!({}), &admin);
    pass(
        "unknown_route_not_found",
        nf.is_err_and(|e| e.starts_with("not found")),
        "no error".into(),
    )?;

    let stranger = Ctx::new("selftest", "x", &["nobody"], None);
    let denied = api.handle(
        "POST",
        "/admin/warehouses",
        &json!({"code": "W", "name": "W"}),
        &stranger,
    );
    pass(
        "permission_denied_without_role",
        denied.is_err_and(|e| e.contains("permission denied")),
        "allowed".into(),
    )?;

    let summary = demo::load_demo(&conn, &ledger, &clock, "selftest")?;
    pass(
        "demo_loads_20_items",
        summary["items"] == 20,
        summary.to_string(),
    )?;
    let open = admin_reports::open_orders(&conn, &admin)?;
    pass(
        "demo_has_one_open_order",
        open["count"] == 1,
        open.to_string(),
    )?;
    let unbalanced = ledger.recorded().iter().any(|e| {
        e.lines.iter().map(|l| l.debit).sum::<i64>()
            != e.lines.iter().map(|l| l.credit).sum::<i64>()
    });
    pass(
        "every_ledger_entry_balances",
        !unbalanced,
        "unbalanced entry".into(),
    )?;
    let rows = common::verify_audit(&conn, "selftest")?;
    pass("audit_chain_verifies", rows > 50, format!("{rows} rows"))?;

    let count = checks.len();
    Ok(json!({"status": "PASS", "scope": "smoke", "count": count, "checks": Value::Object(checks)}))
}

#[cfg(test)]
mod tests {
    #[test]
    fn selftest_passes() {
        let v = super::run_selftest().unwrap();
        assert_eq!(v["status"], "PASS");
        assert!(v["count"].as_u64().unwrap_or(0) >= 8);
    }
}
