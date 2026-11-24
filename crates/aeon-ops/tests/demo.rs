//! The demo loader on a fresh in-memory tenant.

use aeon_ops::admin_reports;
use aeon_ops::common::{self, Ctx, FakeClock, TestLedger};
use aeon_ops::demo::load_demo;
use aeon_ops::schema;

#[test]
fn demo_loads_once_and_everything_balances() {
    let c = common::open_memory().unwrap();
    schema::create_schema(&c).unwrap();
    admin_reports::bootstrap_tenant(&c, "acme", "admin", 1).unwrap();
    let ledger = TestLedger::new();
    let clock = FakeClock::new(1_795_000_000);

    let summary = load_demo(&c, &ledger, &clock, "acme").unwrap();
    assert_eq!(summary["items"], 20);
    assert_eq!(summary["employees"], 5);
    assert_eq!(summary["customers"], 3);

    // The partial sales order and purchase order really exist and are partial.
    let admin = Ctx::new("acme", "admin", &["admin"], Some("ACME"));
    let open = admin_reports::open_orders(&c, &admin).unwrap();
    assert_eq!(open["count"], 1);
    let stock = admin_reports::stock(&c, &admin, None).unwrap();
    assert!(stock["total_value_minor"].as_i64().unwrap() > 0);
    for e in ledger.recorded() {
        let d: i64 = e.lines.iter().map(|l| l.debit).sum();
        let cr: i64 = e.lines.iter().map(|l| l.credit).sum();
        assert_eq!(d, cr, "{}", e.memo);
    }
    assert!(common::verify_audit(&c, "acme").unwrap() > 50);

    // A second load is refused instead of duplicating rows.
    let again = load_demo(&c, &ledger, &clock, "acme").unwrap_err();
    assert!(again.contains("already has demo data"), "{again}");
}

#[test]
fn demo_refuses_tenants_that_were_never_bootstrapped() {
    let c = common::open_memory().unwrap();
    schema::create_schema(&c).unwrap();
    let err = load_demo(&c, &TestLedger::new(), &FakeClock::new(1), "ghost").unwrap_err();
    assert!(err.contains("not bootstrapped"), "{err}");
    assert!(load_demo(&c, &TestLedger::new(), &FakeClock::new(1), " ").is_err());
}
