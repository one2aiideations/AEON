//! End-to-end checks of the main business flows against an in-memory database.

use mouli_ops::common::{self, Ctx, Ledger, TestLedger};
use mouli_ops::{hr, inventory, manufacturing, procurement, projects_service, sales, schema};
use rusqlite::{params, Connection};
use serde_json::Value;

const T: &str = "t1";

fn ts(y: i64, m: i64, d: i64, h: i64, mi: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60
}

fn setup() -> Connection {
    let c = common::open_memory().unwrap();
    schema::create_schema(&c).unwrap();
    for (role, perm) in [
        ("admin", "*"),
        ("clerk", "procurement.requisitions.approve"),
        ("clerk", "procurement.requisitions.create"),
    ] {
        c.execute(
            "INSERT INTO role_permissions VALUES (?1,?2,?3)",
            params![T, role, perm],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO approval_limits VALUES (?1,'admin',-1)",
        params![T],
    )
    .unwrap();
    c.execute(
        "INSERT INTO approval_limits VALUES (?1,'clerk',50000)",
        params![T],
    )
    .unwrap();
    c.execute(
        "INSERT INTO warehouses VALUES (?1,'w1','W1','Main')",
        params![T],
    )
    .unwrap();
    c
}

fn admin() -> Ctx {
    Ctx::new(T, "alice", &["admin"], Some("ACME"))
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

fn item(c: &Connection, code: &str, method: &str, cost: i64, now: i64) -> String {
    id(&inventory::create_item(
        c,
        &admin(),
        &inventory::CreateItem {
            code: code.into(),
            name: code.into(),
            uom: "EA".into(),
            valuation_method: method.into(),
            reorder_point_kth: 0,
            allows_negative: false,
            initial_cost_minor: cost,
        },
        now,
    )
    .unwrap())
}

fn receive(c: &Connection, item_id: &str, qty: i64, cost: i64, now: i64) {
    inventory::receive_stock(
        c,
        &admin(),
        &inventory::ReceiveParams {
            item_id: item_id.into(),
            warehouse_id: "w1".into(),
            qty_kth: qty,
            unit_cost_minor: cost,
            batch_no: None,
            batch_expiry: None,
            ref_type: None,
            ref_id: None,
        },
        now,
    )
    .unwrap();
}

fn balanced(l: &TestLedger) {
    for e in l.recorded() {
        let d: i64 = e.lines.iter().map(|x| x.debit).sum();
        let cr: i64 = e.lines.iter().map(|x| x.credit).sum();
        assert_eq!(d, cr, "{}", e.memo);
    }
}

#[test]
fn sales_flow_quote_to_cash_with_fifo_cogs() {
    let c = setup();
    let now = ts(2026, 3, 2, 10, 0);
    let ledger = TestLedger::new();
    let it = item(&c, "WIDGET", "FIFO", 0, now);
    receive(&c, &it, 10_000, 5000, now);
    receive(&c, &it, 10_000, 6000, now + 1);
    let cust = id(&sales::create_customer(
        &c,
        &admin(),
        &sales::CreateCustomer {
            code: "C1".into(),
            name: "Acme".into(),
            currency: "usd".into(),
            credit_limit_minor: 0,
        },
        now,
    )
    .unwrap());
    let line = sales::OrderLineIn {
        item_id: it.clone(),
        qty_kth: 15_000,
        unit_price_minor: 10_000,
        tax_bps: 1000,
    };
    let order = sales::create_sales_order(
        &c,
        &admin(),
        &cust,
        std::slice::from_ref(&line),
        Some("so-1"),
        now,
    )
    .unwrap();
    let again = sales::create_sales_order(&c, &admin(), &cust, &[line], Some("so-1"), now).unwrap();
    assert_eq!(
        order, again,
        "same idempotency key returns the stored result"
    );
    let oid = id(&order);
    let d1 = id(&sales::create_delivery(
        &c,
        &admin(),
        &oid,
        "w1",
        &[sales::DeliveryLineIn {
            order_line_no: 1,
            qty_kth: 6_000,
        }],
        None,
        now,
    )
    .unwrap());
    let p1 = sales::post_delivery(&c, &admin(), &d1, now).unwrap();
    assert_eq!(p1["cogs_minor"], 30_000);
    assert_eq!(p1["order_status"], "partially_delivered");
    let over = sales::create_delivery(
        &c,
        &admin(),
        &oid,
        "w1",
        &[sales::DeliveryLineIn {
            order_line_no: 1,
            qty_kth: 10_000,
        }],
        None,
        now,
    );
    assert!(over
        .unwrap_err()
        .contains("exceeds the undelivered balance"));
    let d2 = id(&sales::create_delivery(
        &c,
        &admin(),
        &oid,
        "w1",
        &[sales::DeliveryLineIn {
            order_line_no: 1,
            qty_kth: 9_000,
        }],
        None,
        now,
    )
    .unwrap());
    let p2 = sales::post_delivery(&c, &admin(), &d2, now).unwrap();
    assert_eq!(
        p2["cogs_minor"], 50_000,
        "4 from the first lot at 50.00 and 5 from the second at 60.00"
    );
    assert_eq!(p2["order_status"], "delivered");
    assert_eq!(inventory::on_hand(&c, T, &it, "w1").unwrap(), 5_000);
    let inv = sales::create_invoice_from_order(&c, &admin(), &oid, &ledger, None, now).unwrap();
    assert_eq!(inv["net_minor"], 150_000);
    assert_eq!(inv["tax_minor"], 15_000);
    assert_eq!(inv["total_minor"], 165_000);
    let iid = id(&inv);
    assert!(
        sales::create_invoice_from_order(&c, &admin(), &oid, &ledger, None, now).is_err(),
        "one invoice per order"
    );
    let r1 =
        sales::record_receipt(&c, &admin(), &iid, 100_000, "bank", &ledger, None, now).unwrap();
    assert_eq!(r1["invoice_status"], "partially_paid");
    assert!(sales::record_receipt(&c, &admin(), &iid, 65_001, "bank", &ledger, None, now).is_err());
    let r2 = sales::record_receipt(&c, &admin(), &iid, 65_000, "bank", &ledger, None, now).unwrap();
    assert_eq!(r2["invoice_status"], "paid");
    balanced(&ledger);
    assert!(common::verify_audit(&c, T).unwrap() > 8);
}

#[test]
fn other_tenants_see_nothing_and_cannot_act() {
    let c = setup();
    let now = ts(2026, 3, 2, 10, 0);
    let it = item(&c, "WIDGET", "FIFO", 0, now);
    let intruder = Ctx::new("t2", "mallory", &["admin"], None);
    assert!(inventory::receive_stock(
        &c,
        &intruder,
        &inventory::ReceiveParams {
            item_id: it.clone(),
            warehouse_id: "w1".into(),
            qty_kth: 1000,
            unit_cost_minor: 1,
            batch_no: None,
            batch_expiry: None,
            ref_type: None,
            ref_id: None
        },
        now
    )
    .is_err());
    assert!(inventory::stock_on_hand_rows(&c, "t2", None, None)
        .unwrap()
        .is_empty());
    let nobody = Ctx::new(T, "bob", &["nobody"], None);
    let denied = inventory::create_item(
        &c,
        &nobody,
        &inventory::CreateItem {
            code: "X".into(),
            name: "X".into(),
            uom: "EA".into(),
            valuation_method: "FIFO".into(),
            reorder_point_kth: 0,
            allows_negative: false,
            initial_cost_minor: 0,
        },
        now,
    );
    assert!(denied.unwrap_err().contains("permission denied"));
}

#[test]
fn procurement_three_way_match_flags_variance_until_approved() {
    let c = setup();
    let now = ts(2026, 3, 2, 10, 0);
    let ledger = TestLedger::new();
    let it = item(&c, "BOLT", "WA", 0, now);
    let sup = id(&procurement::create_supplier(
        &c,
        &admin(),
        &procurement::CreateSupplier {
            code: "S1".into(),
            name: "Supply".into(),
            currency: "USD".into(),
            payment_terms_days: 30,
        },
        now,
    )
    .unwrap());
    let clerk = Ctx::new(T, "carol", &["clerk"], Some("ACME"));
    let req = id(&procurement::create_requisition(&c, &clerk, &it, 100_000, 1000, now).unwrap());
    let esc = procurement::approve_requisition(&c, &clerk, &req, now).unwrap();
    assert_eq!(
        esc["status"], "escalated",
        "100.00 x 1000 = 100000 is over the clerk limit of 50000"
    );
    let ok = procurement::approve_requisition(&c, &admin(), &req, now).unwrap();
    assert_eq!(ok["status"], "approved");
    let rfq = id(&procurement::create_rfq(&c, &admin(), &req, now).unwrap());
    let q1 = id(&procurement::add_rfq_quote(&c, &admin(), &rfq, &sup, 1000, 7, now).unwrap());
    let ranked = procurement::compare_quotes(&c, T, &rfq).unwrap();
    assert_eq!(ranked.len(), 1);
    procurement::award_rfq(&c, &admin(), &rfq, &q1, now).unwrap();
    let po = id(&procurement::po_from_rfq(&c, &admin(), &rfq, None, now).unwrap());
    let rec = procurement::receive_goods(
        &c,
        &admin(),
        &po,
        "w1",
        &[procurement::GrLineIn {
            po_line_no: 1,
            qty_kth: 60_000,
            batch_no: None,
            batch_expiry: None,
        }],
        &ledger,
        None,
        now,
    )
    .unwrap();
    assert_eq!(rec["po_status"], "partially_received");
    assert_eq!(rec["received_value_minor"], 60_000);
    let toomuch = procurement::receive_goods(
        &c,
        &admin(),
        &po,
        "w1",
        &[procurement::GrLineIn {
            po_line_no: 1,
            qty_kth: 51_000,
            batch_no: None,
            batch_expiry: None,
        }],
        &ledger,
        None,
        now,
    );
    assert!(toomuch.unwrap_err().contains("110%"));
    let bill = procurement::create_supplier_bill(
        &c,
        &admin(),
        "B-1",
        &sup,
        &po,
        66_000,
        "2026-03-02",
        &ledger,
        None,
        now,
    )
    .unwrap();
    assert_eq!(bill["match_status"], "flagged");
    assert_eq!(bill["price_variance_minor"], 6_000);
    assert!(procurement::create_payment_proposal(&c, &admin(), now)
        .unwrap_err()
        .contains("no eligible"));
    procurement::approve_bill_variance(&c, &admin(), &id(&bill), now).unwrap();
    let prop = procurement::create_payment_proposal(&c, &admin(), now).unwrap();
    assert_eq!(prop["total_minor"], 66_000);
    let paid =
        procurement::approve_payment_proposal(&c, &admin(), &id(&prop), &ledger, now).unwrap();
    assert_eq!(paid["status"], "approved");
    balanced(&ledger);
    assert_eq!(inventory::on_hand(&c, T, &it, "w1").unwrap(), 60_000);
}

#[test]
fn manufacturing_work_order_and_mrp() {
    let c = setup();
    let now = ts(2026, 3, 2, 10, 0);
    let ledger = TestLedger::new();
    let a = item(&c, "ASSY", "WA", 0, now);
    let b = item(&c, "PART", "WA", 0, now);
    receive(&c, &b, 100_000, 300, now);
    let bom = id(&manufacturing::create_bom(
        &c,
        &admin(),
        &a,
        0,
        &[manufacturing::BomLineIn {
            component_id: b.clone(),
            qty_kth: 2_000,
            scrap_kth: 0,
        }],
        now,
    )
    .unwrap());
    manufacturing::release_bom(&c, &admin(), &bom, now).unwrap();
    let ex = manufacturing::explode_bom(&c, T, &a, 10_000).unwrap();
    assert_eq!(ex[0]["qty_kth"], 20_000);
    let wo = id(&manufacturing::create_work_order(&c, &admin(), &a, 10_000, "w1", now).unwrap());
    manufacturing::release_work_order(&c, &admin(), &wo, now).unwrap();
    let iss = manufacturing::issue_wo_materials(&c, &admin(), &wo, &ledger, "USD", now).unwrap();
    assert_eq!(iss["total_issued_minor"], 6_000);
    manufacturing::report_output(&c, &admin(), &wo, 10_000, &ledger, "USD", now).unwrap();
    let closed = manufacturing::close_work_order(&c, &admin(), &wo, &ledger, "USD", now).unwrap();
    assert_eq!(closed["variance_minor"], 0);
    assert_eq!(inventory::on_hand(&c, T, &a, "w1").unwrap(), 10_000);
    assert_eq!(inventory::on_hand(&c, T, &b, "w1").unwrap(), 80_000);
    balanced(&ledger);
    let cust = id(&sales::create_customer(
        &c,
        &admin(),
        &sales::CreateCustomer {
            code: "C1".into(),
            name: "Acme".into(),
            currency: "USD".into(),
            credit_limit_minor: 0,
        },
        now,
    )
    .unwrap());
    sales::create_sales_order(
        &c,
        &admin(),
        &cust,
        &[sales::OrderLineIn {
            item_id: a.clone(),
            qty_kth: 30_000,
            unit_price_minor: 100,
            tax_bps: 0,
        }],
        None,
        now,
    )
    .unwrap();
    let plan = manufacturing::mrp_run(&c, &admin(), now).unwrap();
    let props = plan["proposals"].as_array().unwrap();
    assert!(
        props.iter().any(|p| p["item_id"] == a.as_str()
            && p["kind"] == "work_order"
            && p["qty_kth"] == 20_000),
        "{plan}"
    );
}

#[test]
fn payroll_prorates_a_mid_month_joiner_and_leave_accrues_across_years() {
    let c = setup();
    let now = ts(2026, 4, 1, 9, 0);
    let ledger = TestLedger::new();
    let dep = id(&hr::create_department(&c, &admin(), "ENG", "Engineering", now).unwrap());
    let emp = id(&hr::create_employee(
        &c,
        &admin(),
        "E1",
        "Ada",
        "Lovelace",
        &dep,
        None,
        "2026-03-16",
        310_000,
        "USD",
        now,
    )
    .unwrap());
    let old = id(&hr::create_employee(
        &c,
        &admin(),
        "E2",
        "Alan",
        "Turing",
        &dep,
        None,
        "2025-06-01",
        300_000,
        "USD",
        now,
    )
    .unwrap());
    hr::create_payroll_rule(
        &c,
        &admin(),
        &hr::CreatePayrollRule {
            code: "TAX".into(),
            name: "Tax".into(),
            kind: "percent_of_gross".into(),
            rate_kth: Some(10_000),
            amount_minor: None,
            employer_side: false,
            order_idx: 1,
        },
        now,
    )
    .unwrap();
    let run = hr::run_payroll(&c, &admin(), "2026-03", &ledger, Some("pay-2026-03"), now).unwrap();
    assert_eq!(run["employees"], 2);
    assert!(
        hr::run_payroll(&c, &admin(), "2026-03", &ledger, None, now).is_err(),
        "one run per period"
    );
    let sum = hr::payroll_summary(&c, T, "2026-03").unwrap();
    // 16 of 31 days: proration 516 thousandths of 3100.00 = 1599.60; the other employee earns 3000.00 in full.
    assert_eq!(sum["gross_minor"], 159_960 + 300_000);
    assert_eq!(sum["deductions_minor"], 15_996 + 30_000);
    balanced(&ledger);
    let lt = id(&hr::create_leave_type(&c, &admin(), "AL", "Annual", 2_000, now).unwrap());
    hr::accrue_leave(&c, &admin(), "2025-12", now).unwrap();
    hr::accrue_leave(&c, &admin(), "2026-01", now).unwrap();
    hr::accrue_leave(&c, &admin(), "2026-01", now).unwrap();
    let bal: i64 = c.query_row("SELECT balance_kth FROM leave_balances WHERE tenant_id=?1 AND employee_id=?2 AND leave_type_id=?3", params![T, old, lt], |r| r.get(0)).unwrap();
    assert_eq!(bal, 4_000, "two months accrued once each");
    assert!(
        hr::take_leave(&c, &admin(), &old, &lt, 5_000, "2026-02-02", now)
            .unwrap_err()
            .contains("insufficient leave balance")
    );
    hr::take_leave(&c, &admin(), &old, &lt, 1_000, "2026-02-02", now).unwrap();
    let _ = emp;
}

#[test]
fn sla_clock_pauses_across_a_weekend_and_breaches_once() {
    let c = setup();
    let fri = ts(2026, 11, 27, 16, 0);
    let pol =
        id(&projects_service::create_sla_policy(&c, &admin(), "Std", 60, 480, 240, fri).unwrap());
    let tk =
        id(
            &projects_service::create_ticket(&c, &admin(), "Printer", None, "normal", &pol, fri)
                .unwrap(),
        );
    let paused =
        projects_service::pause_ticket(&c, &admin(), &tk, ts(2026, 11, 27, 16, 30)).unwrap();
    assert_eq!(paused["sla_accum_minutes"], 30);
    projects_service::resume_ticket(&c, &admin(), &tk, ts(2026, 11, 30, 9, 0)).unwrap();
    let mid = projects_service::check_sla(&c, &admin(), &tk, ts(2026, 11, 30, 15, 0)).unwrap();
    assert_eq!(mid["elapsed_minutes"], 390);
    assert_eq!(mid["escalated"], true);
    assert_eq!(mid["breached"], false);
    let late = projects_service::check_sla(&c, &admin(), &tk, ts(2026, 11, 30, 17, 0)).unwrap();
    assert_eq!(late["elapsed_minutes"], 510);
    assert_eq!(late["breached"], true);
    let after = projects_service::check_sla(&c, &admin(), &tk, ts(2026, 12, 1, 12, 0)).unwrap();
    assert_eq!(after["breached"], false);
    assert_eq!(after["already_breached"], true);
}

#[test]
fn tampering_with_the_audit_log_is_detected() {
    let c = setup();
    let now = ts(2026, 3, 2, 10, 0);
    item(&c, "A", "FIFO", 0, now);
    item(&c, "B", "FIFO", 0, now);
    item(&c, "C", "FIFO", 0, now);
    assert_eq!(common::verify_audit(&c, T).unwrap(), 3);
    c.execute(
        "UPDATE audit_log SET payload='{\"code\":\"HACKED\"}' WHERE tenant_id=?1 AND seq=2",
        params![T],
    )
    .unwrap();
    let err = common::verify_audit(&c, T).unwrap_err();
    assert!(err.contains("seq 2"), "{err}");
}

#[test]
fn twenty_threads_never_oversell_one_item() {
    use std::sync::Arc;
    let dir = std::env::temp_dir().join(format!("mouli-ops-conc-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let path = dir.to_string_lossy().to_string();
    {
        let c = common::open_db(&path).unwrap();
        schema::create_schema(&c).unwrap();
        c.execute(
            "INSERT INTO role_permissions VALUES (?1,'admin','*')",
            params![T],
        )
        .unwrap();
        c.execute(
            "INSERT INTO warehouses VALUES (?1,'w1','W1','Main')",
            params![T],
        )
        .unwrap();
        let it = item(&c, "HOT", "FIFO", 0, 1);
        receive(&c, &it, 10_000, 100, 1);
        c.execute("UPDATE items SET code='HOT' WHERE id=?1", params![it])
            .unwrap();
    }
    let path = Arc::new(path);
    let item_id: String = {
        let c = common::open_db(&path).unwrap();
        c.query_row("SELECT id FROM items WHERE tenant_id='t1'", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let mut handles = Vec::new();
    for n in 0..20 {
        let path = Arc::clone(&path);
        let item_id = item_id.clone();
        handles.push(std::thread::spawn(move || {
            let c = common::open_db(&path).unwrap();
            c.busy_timeout(std::time::Duration::from_secs(30)).unwrap();
            inventory::issue_stock(
                &c,
                &admin(),
                &inventory::IssueParams {
                    item_id,
                    warehouse_id: "w1".into(),
                    qty_kth: 1_000,
                    ref_type: "test".into(),
                    ref_id: format!("t{n}"),
                },
                2,
            )
            .is_ok()
        }));
    }
    let ok = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|x| *x)
        .count();
    let c = common::open_db(&path).unwrap();
    assert_eq!(ok, 10, "exactly the ten units on hand are issued");
    assert_eq!(inventory::on_hand(&c, T, &item_id, "w1").unwrap(), 0);
    let _ = std::fs::remove_file(&*path);
}

#[test]
fn ledger_trait_rejects_unbalanced_entries() {
    let l = TestLedger::new();
    let bad = common::JournalEntry {
        date: "2026-01-01".into(),
        memo: "bad".into(),
        currency: "USD".into(),
        lines: vec![common::JournalLine {
            account: "1000".into(),
            debit: 5,
            credit: 0,
        }],
    };
    assert!(l.post(T, &bad).is_err());
}

#[test]
fn quotation_and_order_totals_use_thousandths_of_a_unit() {
    let c = setup();
    let now = ts(2026, 3, 2, 10, 0);
    let it = item(&c, "WIDGET", "FIFO", 0, now);
    let cust = id(&sales::create_customer(
        &c,
        &admin(),
        &sales::CreateCustomer {
            code: "C1".into(),
            name: "Acme".into(),
            currency: "USD".into(),
            credit_limit_minor: 0,
        },
        now,
    )
    .unwrap());
    // 2 units at 50.00 with 10% line discount and 10% tax: 100.00 - 10.00 = 90.00, tax 9.00, total 99.00.
    let q = sales::create_quotation(
        &c,
        &admin(),
        &sales::CreateQuotation {
            customer_id: cust.clone(),
            opportunity_id: None,
            lines: vec![sales::QuoteLineIn {
                item_id: Some(it.clone()),
                description: "w".into(),
                qty_kth: 2_000,
                unit_price_minor: 5_000,
                discount_pct_kth: 10_000,
                tax_bps: 1000,
            }],
            discount_minor: 0,
            tax_bps: 0,
            expiry_days: 30,
        },
        None,
        now,
    )
    .unwrap();
    assert_eq!(q["subtotal_minor"], 9_000);
    assert_eq!(q["tax_minor"], 900);
    assert_eq!(q["total_minor"], 9_900);
    // 20 units at 180.00 with 10% tax is 3,960.00 (396,000 minor), not 396,000,000.
    let o = sales::create_sales_order(
        &c,
        &admin(),
        &cust,
        &[sales::OrderLineIn {
            item_id: it,
            qty_kth: 20_000,
            unit_price_minor: 18_000,
            tax_bps: 1000,
        }],
        None,
        now,
    )
    .unwrap();
    assert_eq!(o["total_minor"], 396_000);
}
