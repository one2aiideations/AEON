//! Built-in self-test behind `mouli-ops test`: 150+ named checks over every module.

use crate::common::{self, Ctx, FakeClock, TestLedger};
use crate::{
    admin_reports, api, hr, inventory, manufacturing, procurement, projects_service, sales, server,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::Mutex;

fn ts(y: i64, m: i64, d: i64, h: i64, mi: i64) -> i64 {
    let mut yy = y;
    let mut mm = m;
    if mm <= 2 {
        yy -= 1;
        mm += 12;
    }
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = (mm + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + h * 3_600 + mi * 60
}

struct Env {
    conn: Connection,
    ledger: TestLedger,
    clock: FakeClock,
    admin: Ctx,
    ids: std::collections::HashMap<String, String>,
    results: Vec<(String, String)>,
}

impl Env {
    fn id(&self, key: &str) -> Result<String, String> {
        self.ids
            .get(key)
            .cloned()
            .ok_or_else(|| format!("missing id '{key}'"))
    }
    fn set_id(&mut self, key: &str, value: &str) {
        self.ids.insert(key.to_string(), value.to_string());
    }
}

fn expect_eq<T: PartialEq + std::fmt::Debug>(
    env: &mut Env,
    name: &str,
    actual: T,
    expected: T,
) -> Result<(), String> {
    if actual == expected {
        env.results.push((name.to_string(), "PASS".to_string()));
        Ok(())
    } else {
        Err(format!(
            "check '{name}' failed: expected {expected:?}, got {actual:?}"
        ))
    }
}

fn expect_ok<T: std::fmt::Debug>(
    env: &mut Env,
    name: &str,
    r: common::Res<T>,
) -> Result<T, String> {
    match r {
        Ok(v) => {
            env.results.push((name.to_string(), "PASS".to_string()));
            Ok(v)
        }
        Err(e) => Err(format!("check '{name}' failed: {e}")),
    }
}

fn expect_err<T: std::fmt::Debug>(
    env: &mut Env,
    name: &str,
    r: common::Res<T>,
) -> Result<(), String> {
    match r {
        Ok(v) => Err(format!(
            "check '{name}' failed: expected an error, got Ok({v:?})"
        )),
        Err(_) => {
            env.results.push((name.to_string(), "PASS".to_string()));
            Ok(())
        }
    }
}

fn expect_err_contains<T: std::fmt::Debug>(
    env: &mut Env,
    name: &str,
    r: common::Res<T>,
    needle: &str,
) -> Result<(), String> {
    match r {
        Ok(v) => Err(format!(
            "check '{name}' failed: expected error containing '{needle}', got Ok({v:?})"
        )),
        Err(e) => {
            if e.contains(needle) {
                env.results.push((name.to_string(), "PASS".to_string()));
                Ok(())
            } else {
                Err(format!(
                    "check '{name}' failed: error '{e}' does not contain '{needle}'"
                ))
            }
        }
    }
}

fn json_id(v: &Value) -> Result<String, String> {
    common::jstr(v, "id")
}

fn has_entry_with_accounts(entries: &[common::JournalEntry], accounts: &[&str]) -> bool {
    let mut want = accounts.to_vec();
    want.sort_unstable();
    entries.iter().any(|e| {
        let mut got: Vec<&str> = e.lines.iter().map(|l| l.account.as_str()).collect();
        got.sort_unstable();
        got == want
    })
}

fn entries_snapshot(env: &Env) -> Vec<common::JournalEntry> {
    env.ledger
        .entries
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn clerk_ctx() -> Ctx {
    Ctx {
        tenant: "acme".to_string(),
        user: "clerk".to_string(),
        roles: vec!["clerk".to_string()],
        company: None,
    }
}

fn other_ctx() -> Ctx {
    Ctx {
        tenant: "other".to_string(),
        user: "boss".to_string(),
        roles: vec!["admin".to_string()],
        company: None,
    }
}

fn recv(item: &str, wh: &str, qty_kth: i64, cost: i64) -> inventory::ReceiveParams {
    inventory::ReceiveParams {
        item_id: item.to_string(),
        warehouse_id: wh.to_string(),
        qty_kth,
        unit_cost_minor: cost,
        batch_no: None,
        batch_expiry: None,
        ref_type: None,
        ref_id: None,
    }
}

fn issue(item: &str, wh: &str, qty_kth: i64, ref_id: &str) -> inventory::IssueParams {
    inventory::IssueParams {
        item_id: item.to_string(),
        warehouse_id: wh.to_string(),
        qty_kth,
        ref_type: "manual".to_string(),
        ref_id: ref_id.to_string(),
    }
}

fn new_item(
    code: &str,
    name: &str,
    method: &str,
    reorder: i64,
    cost: i64,
) -> inventory::CreateItem {
    inventory::CreateItem {
        code: code.to_string(),
        name: name.to_string(),
        uom: "EA".to_string(),
        valuation_method: method.to_string(),
        reorder_point_kth: reorder,
        allows_negative: false,
        initial_cost_minor: cost,
    }
}

fn new_customer(code: &str, name: &str, currency: &str, limit: i64) -> sales::CreateCustomer {
    sales::CreateCustomer {
        code: code.to_string(),
        name: name.to_string(),
        currency: currency.to_string(),
        credit_limit_minor: limit,
    }
}

fn part1(env: &mut Env) -> Result<(), String> {
    let now = ts(2026, 11, 1, 9, 0);
    let ctx = env.admin.clone();
    let wa = expect_ok(
        env,
        "create_warehouse_a",
        admin_reports::create_warehouse(&env.conn, &ctx, "WH-A", "Main warehouse", now),
    )?;
    env.set_id("wh_a", &json_id(&wa)?);
    let wb = expect_ok(
        env,
        "create_warehouse_b",
        admin_reports::create_warehouse(&env.conn, &ctx, "WH-B", "Secondary warehouse", now),
    )?;
    env.set_id("wh_b", &json_id(&wb)?);
    let whs = expect_ok(
        env,
        "list_warehouses",
        admin_reports::list_warehouses(&env.conn, "acme"),
    )?;
    expect_eq(env, "list_warehouses_count", whs.len(), 2)?;
    expect_ok(
        env,
        "admin_perm_items_create",
        common::require_perm(&env.conn, &ctx, "inventory.items.create"),
    )?;
    expect_ok(
        env,
        "grant_clerk_create",
        admin_reports::grant_permission(
            &env.conn,
            &ctx,
            "clerk",
            "procurement.requisitions.create",
            now,
        ),
    )?;
    expect_ok(
        env,
        "grant_clerk_approve",
        admin_reports::grant_permission(
            &env.conn,
            &ctx,
            "clerk",
            "procurement.requisitions.approve",
            now,
        ),
    )?;
    expect_ok(
        env,
        "set_clerk_limit",
        admin_reports::set_approval_limit(&env.conn, &ctx, "clerk", 50_000, now),
    )?;
    let clerk = clerk_ctx();
    expect_ok(
        env,
        "clerk_perm_create",
        common::require_perm(&env.conn, &clerk, "procurement.requisitions.create"),
    )?;
    expect_ok(
        env,
        "clerk_perm_approve",
        common::require_perm(&env.conn, &clerk, "procurement.requisitions.approve"),
    )?;
    let item = expect_ok(
        env,
        "create_item_fifo",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("A-100", "Widget FIFO", "FIFO", 5_000, 5_000),
            now,
        ),
    )?;
    env.set_id("item_a", &json_id(&item)?);
    let itemw = expect_ok(
        env,
        "create_item_wa",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("A-200", "Widget WA", "WA", 0, 1_000),
            now,
        ),
    )?;
    env.set_id("item_wa", &json_id(&itemw)?);
    expect_err_contains(
        env,
        "item_bad_valuation",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("A-BAD", "Bad", "fifo", 0, 1_000),
            now,
        ),
        "",
    )?;
    expect_err_contains(
        env,
        "clerk_cannot_create_item",
        inventory::create_item(
            &env.conn,
            &clerk,
            &new_item("A-NO", "No", "FIFO", 0, 1_000),
            now,
        ),
        "permission",
    )?;
    expect_err_contains(
        env,
        "clerk_cannot_create_warehouse",
        admin_reports::create_warehouse(&env.conn, &clerk, "WH-NO", "No", now),
        "permission",
    )?;
    expect_err_contains(
        env,
        "clerk_cannot_grant",
        admin_reports::grant_permission(&env.conn, &clerk, "clerk", "inventory.items.create", now),
        "permission",
    )?;
    expect_err_contains(
        env,
        "clerk_cannot_set_limit",
        admin_reports::set_approval_limit(&env.conn, &clerk, "clerk", 1, now),
        "permission",
    )?;
    expect_err_contains(
        env,
        "clerk_cannot_revoke",
        admin_reports::revoke_permission(
            &env.conn,
            &clerk,
            "clerk",
            "procurement.requisitions.create",
            now,
        ),
        "permission",
    )?;
    expect_ok(
        env,
        "approval_under_limit",
        common::check_approval(&env.conn, &clerk, 40_000),
    )?;
    expect_err_contains(
        env,
        "approval_over_limit",
        common::check_approval(&env.conn, &clerk, 60_000),
        "limit",
    )?;
    let (ia, wha) = (env.id("item_a")?, env.id("wh_a")?);
    expect_err(
        env,
        "receive_zero_qty",
        inventory::receive_stock(&env.conn, &ctx, &recv(&ia, &wha, 0, 5_000), now),
    )?;
    expect_err(
        env,
        "receive_negative_qty",
        inventory::receive_stock(&env.conn, &ctx, &recv(&ia, &wha, -1_000, 5_000), now),
    )?;
    let cust = expect_ok(
        env,
        "create_customer",
        sales::create_customer(
            &env.conn,
            &ctx,
            &new_customer("C-001", "Buyco", "USD", 1_000_000),
            now,
        ),
    )?;
    env.set_id("customer", &json_id(&cust)?);
    expect_err_contains(
        env,
        "customer_currency_2_letters",
        sales::create_customer(
            &env.conn,
            &ctx,
            &new_customer("C-002", "Bad2", "US", 1),
            now,
        ),
        "",
    )?;
    expect_err_contains(
        env,
        "customer_currency_4_letters",
        sales::create_customer(
            &env.conn,
            &ctx,
            &new_customer("C-003", "Bad4", "USDD", 1),
            now,
        ),
        "",
    )?;
    expect_err(
        env,
        "unknown_customer_order",
        sales::create_sales_order(
            &env.conn,
            &ctx,
            "no-such-customer",
            &[sales::OrderLineIn {
                item_id: ia.clone(),
                qty_kth: 1_000,
                unit_price_minor: 100,
                tax_bps: 0,
            }],
            None,
            now,
        ),
    )?;
    let customer = env.id("customer")?;
    expect_err(
        env,
        "quotation_zero_qty",
        sales::create_quotation(
            &env.conn,
            &ctx,
            &sales::CreateQuotation {
                customer_id: customer,
                opportunity_id: None,
                lines: vec![sales::QuoteLineIn {
                    item_id: Some(ia.clone()),
                    description: "q".to_string(),
                    qty_kth: 0,
                    unit_price_minor: 100,
                    discount_pct_kth: 0,
                    tax_bps: 0,
                }],
                discount_minor: 0,
                tax_bps: 0,
                expiry_days: 7,
            },
            None,
            now,
        ),
    )?;
    expect_err(
        env,
        "issue_zero_qty",
        inventory::issue_material(&env.conn, &ctx, &ia, &wha, 0, "manual", "z", now),
    )?;
    Ok(())
}

fn part2(env: &mut Env) -> Result<(), String> {
    let now = ts(2026, 11, 2, 9, 0);
    let ctx = env.admin.clone();
    let item = env.id("item_a")?;
    let wh = env.id("wh_a")?;
    let customer = env.id("customer")?;
    expect_ok(
        env,
        "scenario_a_recv_1",
        inventory::receive_stock(&env.conn, &ctx, &recv(&item, &wh, 10_000, 5_000), now),
    )?;
    let oh = expect_ok(
        env,
        "on_hand_after_recv1",
        inventory::on_hand(&env.conn, "acme", &item, &wh),
    )?;
    expect_eq(env, "on_hand_equals_10", oh, 10_000)?;
    expect_ok(
        env,
        "scenario_a_recv_2",
        inventory::receive_stock(&env.conn, &ctx, &recv(&item, &wh, 10_000, 6_000), now + 60),
    )?;
    let v = expect_ok(
        env,
        "stock_value_after_recv",
        inventory::stock_value_minor(&env.conn, "acme", Some(&wh)),
    )?;
    expect_eq(env, "stock_value_110000", v, 110_000)?;
    let line = || sales::OrderLineIn {
        item_id: item.clone(),
        qty_kth: 15_000,
        unit_price_minor: 10_000,
        tax_bps: 1_000,
    };
    let order = expect_ok(
        env,
        "create_sales_order",
        sales::create_sales_order(
            &env.conn,
            &ctx,
            &customer,
            &[line()],
            Some("SO-A-001"),
            now + 120,
        ),
    )?;
    let order_id = json_id(&order)?;
    env.set_id("order", &order_id);
    let order2 = expect_ok(
        env,
        "order_idem_replay",
        sales::create_sales_order(
            &env.conn,
            &ctx,
            &customer,
            &[line()],
            Some("SO-A-001"),
            now + 130,
        ),
    )?;
    expect_eq(
        env,
        "order_idem_same_id",
        json_id(&order2)?,
        order_id.clone(),
    )?;
    let open = expect_ok(env, "open_orders", sales::open_orders(&env.conn, "acme"))?;
    expect_eq(env, "open_orders_len", open.len(), 1)?;
    let d1 = expect_ok(
        env,
        "create_delivery_1",
        sales::create_delivery(
            &env.conn,
            &ctx,
            &order_id,
            &wh,
            &[sales::DeliveryLineIn {
                order_line_no: 1,
                qty_kth: 6_000,
            }],
            None,
            now + 240,
        ),
    )?;
    let d1_id = json_id(&d1)?;
    expect_ok(
        env,
        "post_delivery_1",
        sales::post_delivery(&env.conn, &ctx, &d1_id, now + 250),
    )?;
    let v = expect_ok(
        env,
        "stock_value_after_d1",
        inventory::stock_value_minor(&env.conn, "acme", Some(&wh)),
    )?;
    expect_eq(env, "stock_value_80000_after_d1", v, 80_000)?;
    let oh = expect_ok(
        env,
        "on_hand_after_d1",
        inventory::on_hand(&env.conn, "acme", &item, &wh),
    )?;
    expect_eq(env, "on_hand_14_after_d1", oh, 14_000)?;
    expect_err_contains(
        env,
        "over_delivery_refused",
        sales::create_delivery(
            &env.conn,
            &ctx,
            &order_id,
            &wh,
            &[sales::DeliveryLineIn {
                order_line_no: 1,
                qty_kth: 9_001,
            }],
            None,
            now + 290,
        ),
        "exceeds the undelivered balance",
    )?;
    let d2 = expect_ok(
        env,
        "create_delivery_2",
        sales::create_delivery(
            &env.conn,
            &ctx,
            &order_id,
            &wh,
            &[sales::DeliveryLineIn {
                order_line_no: 1,
                qty_kth: 9_000,
            }],
            None,
            now + 300,
        ),
    )?;
    let d2_id = json_id(&d2)?;
    expect_ok(
        env,
        "post_delivery_2",
        sales::post_delivery(&env.conn, &ctx, &d2_id, now + 310),
    )?;
    let v = expect_ok(
        env,
        "stock_value_after_d2",
        inventory::stock_value_minor(&env.conn, "acme", Some(&wh)),
    )?;
    expect_eq(env, "stock_value_30000_after_d2", v, 30_000)?;
    let oh = expect_ok(
        env,
        "on_hand_after_d2",
        inventory::on_hand(&env.conn, "acme", &item, &wh),
    )?;
    expect_eq(env, "on_hand_5_after_d2", oh, 5_000)?;
    expect_err_contains(
        env,
        "delivered_order_refuses_more",
        sales::create_delivery(
            &env.conn,
            &ctx,
            &order_id,
            &wh,
            &[sales::DeliveryLineIn {
                order_line_no: 1,
                qty_kth: 1_000,
            }],
            None,
            now + 320,
        ),
        "cannot be delivered",
    )?;
    let inv = expect_ok(
        env,
        "create_invoice",
        sales::create_invoice_from_order(
            &env.conn,
            &ctx,
            &order_id,
            &env.ledger,
            Some("INV-A-001"),
            now + 400,
        ),
    )?;
    let invoice_id = json_id(&inv)?;
    env.set_id("invoice", &invoice_id);
    let invs = inv.to_string();
    expect_eq(env, "invoice_net_150000", invs.contains("150000"), true)?;
    expect_eq(env, "invoice_tax_15000", invs.contains("15000"), true)?;
    expect_eq(env, "invoice_total_165000", invs.contains("165000"), true)?;
    expect_err(
        env,
        "second_invoice_refused",
        sales::create_invoice_from_order(
            &env.conn,
            &ctx,
            &order_id,
            &env.ledger,
            Some("INV-A-002"),
            now + 410,
        ),
    )?;
    let r1 = expect_ok(
        env,
        "record_receipt_1",
        sales::record_receipt(
            &env.conn,
            &ctx,
            &invoice_id,
            100_000,
            "cash",
            &env.ledger,
            Some("RCPT-A-1"),
            now + 500,
        ),
    )?;
    let r1b = expect_ok(
        env,
        "receipt_idem_replay",
        sales::record_receipt(
            &env.conn,
            &ctx,
            &invoice_id,
            100_000,
            "cash",
            &env.ledger,
            Some("RCPT-A-1"),
            now + 510,
        ),
    )?;
    expect_eq(env, "receipt_idem_same", json_id(&r1b)?, json_id(&r1)?)?;
    expect_err_contains(
        env,
        "receipt_over_outstanding",
        sales::record_receipt(
            &env.conn,
            &ctx,
            &invoice_id,
            65_001,
            "cash",
            &env.ledger,
            None,
            now + 520,
        ),
        "exceeds the outstanding balance",
    )?;
    expect_ok(
        env,
        "record_receipt_2",
        sales::record_receipt(
            &env.conn,
            &ctx,
            &invoice_id,
            65_000,
            "cash",
            &env.ledger,
            Some("RCPT-A-2"),
            now + 530,
        ),
    )?;
    expect_err_contains(
        env,
        "receipt_when_paid_refused",
        sales::record_receipt(
            &env.conn,
            &ctx,
            &invoice_id,
            1,
            "cash",
            &env.ledger,
            None,
            now + 540,
        ),
        "exceeds the outstanding balance",
    )?;
    expect_ok(
        env,
        "adjust_up",
        inventory::adjust_stock(&env.conn, &ctx, &item, &wh, 2_000, "count", now + 600),
    )?;
    let oh = expect_ok(
        env,
        "on_hand_after_adjust_up",
        inventory::on_hand(&env.conn, "acme", &item, &wh),
    )?;
    expect_eq(env, "on_hand_7_after_adjust_up", oh, 7_000)?;
    expect_ok(
        env,
        "adjust_down",
        inventory::adjust_stock(&env.conn, &ctx, &item, &wh, -2_000, "count", now + 610),
    )?;
    let oh = expect_ok(
        env,
        "on_hand_after_adjust_down",
        inventory::on_hand(&env.conn, "acme", &item, &wh),
    )?;
    expect_eq(env, "on_hand_5_final", oh, 5_000)?;
    expect_ok(
        env,
        "reserve_two_units",
        inventory::reserve_stock(
            &env.conn,
            &ctx,
            &item,
            &wh,
            2_000,
            "sales_order",
            &order_id,
            now + 620,
        ),
    )?;
    let rq = expect_ok(
        env,
        "reserved_qty",
        inventory::reserved_qty(&env.conn, "acme", &item, &wh),
    )?;
    expect_eq(env, "reserved_is_2", rq, 2_000)?;
    let dvr = expect_ok(
        env,
        "delivered_vs_ordered",
        sales::delivered_vs_ordered(&env.conn, "acme", &order_id),
    )?;
    expect_eq(env, "delivered_rows", dvr.len(), 1)?;
    let v = expect_ok(
        env,
        "stock_value_final",
        inventory::stock_value_minor(&env.conn, "acme", Some(&wh)),
    )?;
    // FIFO: +2 at 50.00 then -2 takes the oldest layer (60.00): 3 x 60.00 + 2 x 50.00 = 28,000.
    expect_eq(env, "stock_value_28000_final", v, 28_000)?;
    Ok(())
}

fn part3(env: &mut Env) -> Result<(), String> {
    let now = ts(2026, 11, 3, 9, 0);
    let ctx = env.admin.clone();
    let clerk = clerk_ctx();
    let supplier_spec = |code: &str, name: &str, days: i64| procurement::CreateSupplier {
        code: code.to_string(),
        name: name.to_string(),
        currency: "USD".to_string(),
        payment_terms_days: days,
    };
    let supplier = expect_ok(
        env,
        "create_supplier",
        procurement::create_supplier(
            &env.conn,
            &ctx,
            &supplier_spec("SUP-1", "Supplies Co", 30),
            now,
        ),
    )?;
    let supplier_id = json_id(&supplier)?;
    env.set_id("supplier", &supplier_id);
    let supplier2 = expect_ok(
        env,
        "create_supplier_2",
        procurement::create_supplier(
            &env.conn,
            &ctx,
            &supplier_spec("SUP-2", "Other Supplies", 14),
            now,
        ),
    )?;
    let supplier2_id = json_id(&supplier2)?;
    let pitem = expect_ok(
        env,
        "create_procurement_item",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("P-PROC", "Procured part", "FIFO", 0, 5_000),
            now,
        ),
    )?;
    let pitem_id = json_id(&pitem)?;
    env.set_id("item_proc", &pitem_id);
    let big_req = expect_ok(
        env,
        "clerk_creates_requisition",
        procurement::create_requisition(&env.conn, &clerk, &pitem_id, 1_000, 60_000, now),
    )?;
    let big_req_id = json_id(&big_req)?;
    let escal = expect_ok(
        env,
        "clerk_approve_over_limit",
        procurement::approve_requisition(&env.conn, &clerk, &big_req_id, now + 10),
    )?;
    expect_eq(
        env,
        "over_limit_escalated",
        common::jstr(&escal, "status")?,
        "escalated".to_string(),
    )?;
    let notifs = expect_ok(
        env,
        "procurement_manager_notified",
        admin_reports::list_notifications(&env.conn, "acme", "procurement-manager", true, 10),
    )?;
    expect_eq(env, "notification_queued", !notifs.is_empty(), true)?;
    let approved = expect_ok(
        env,
        "admin_approves_escalated",
        procurement::approve_requisition(&env.conn, &ctx, &big_req_id, now + 20),
    )?;
    expect_eq(
        env,
        "escalated_then_approved",
        common::jstr(&approved, "status")?,
        "approved".to_string(),
    )?;
    let small_req = expect_ok(
        env,
        "clerk_small_requisition",
        procurement::create_requisition(&env.conn, &clerk, &pitem_id, 1_000, 30_000, now + 30),
    )?;
    let small_req_id = json_id(&small_req)?;
    let small_ok = expect_ok(
        env,
        "clerk_approve_under_limit",
        procurement::approve_requisition(&env.conn, &clerk, &small_req_id, now + 40),
    )?;
    expect_eq(
        env,
        "under_limit_approved",
        common::jstr(&small_ok, "status")?,
        "approved".to_string(),
    )?;
    let rfq = expect_ok(
        env,
        "create_rfq",
        procurement::create_rfq(&env.conn, &ctx, &small_req_id, now + 50),
    )?;
    let rfq_id = json_id(&rfq)?;
    expect_ok(
        env,
        "add_quote_1",
        procurement::add_rfq_quote(&env.conn, &ctx, &rfq_id, &supplier_id, 9_500, 14, now + 60),
    )?;
    let q2 = expect_ok(
        env,
        "add_quote_2",
        procurement::add_rfq_quote(&env.conn, &ctx, &rfq_id, &supplier2_id, 9_000, 21, now + 70),
    )?;
    let q2_id = json_id(&q2)?;
    let quotes = expect_ok(
        env,
        "compare_quotes",
        procurement::compare_quotes(&env.conn, "acme", &rfq_id),
    )?;
    expect_eq(env, "compare_quotes_len", quotes.len(), 2)?;
    expect_ok(
        env,
        "award_rfq",
        procurement::award_rfq(&env.conn, &ctx, &rfq_id, &q2_id, now + 80),
    )?;
    let po_line = || procurement::PoLineIn {
        item_id: pitem_id.clone(),
        qty_kth: 10_000,
        unit_cost_minor: 5_000,
    };
    let po = expect_ok(
        env,
        "create_po",
        procurement::create_po(
            &env.conn,
            &ctx,
            &supplier_id,
            &[po_line()],
            Some("2026-12-15"),
            Some("PO-001"),
            now + 90,
        ),
    )?;
    let po_id = json_id(&po)?;
    env.set_id("po", &po_id);
    let po_replay = expect_ok(
        env,
        "po_idem_replay",
        procurement::create_po(
            &env.conn,
            &ctx,
            &supplier_id,
            &[po_line()],
            Some("2026-12-15"),
            Some("PO-001"),
            now + 100,
        ),
    )?;
    expect_eq(env, "po_idem_same_id", json_id(&po_replay)?, po_id.clone())?;
    let wh_a = env.id("wh_a")?;
    let gr = |qty: i64| procurement::GrLineIn {
        po_line_no: 1,
        qty_kth: qty,
        batch_no: None,
        batch_expiry: None,
    };
    expect_ok(
        env,
        "receive_goods_partial",
        procurement::receive_goods(
            &env.conn,
            &ctx,
            &po_id,
            &wh_a,
            &[gr(9_000)],
            &env.ledger,
            Some("GR-001"),
            now + 110,
        ),
    )?;
    expect_err_contains(
        env,
        "over_receipt_110",
        procurement::receive_goods(
            &env.conn,
            &ctx,
            &po_id,
            &wh_a,
            &[gr(2_500)],
            &env.ledger,
            None,
            now + 115,
        ),
        "110%",
    )?;
    expect_ok(
        env,
        "receive_goods_rest",
        procurement::receive_goods(
            &env.conn,
            &ctx,
            &po_id,
            &wh_a,
            &[gr(1_000)],
            &env.ledger,
            Some("GR-002"),
            now + 120,
        ),
    )?;
    expect_err_contains(
        env,
        "closed_po_refuses_receipt",
        procurement::receive_goods(
            &env.conn,
            &ctx,
            &po_id,
            &wh_a,
            &[gr(100)],
            &env.ledger,
            None,
            now + 125,
        ),
        "to receive goods",
    )?;
    let bill = expect_ok(
        env,
        "create_supplier_bill",
        procurement::create_supplier_bill(
            &env.conn,
            &ctx,
            "BILL-0001",
            &supplier_id,
            &po_id,
            55_000,
            "2026-11-20",
            &env.ledger,
            Some("BILL-001"),
            now + 130,
        ),
    )?;
    let bill_id = json_id(&bill)?;
    env.set_id("bill", &bill_id);
    expect_eq(
        env,
        "bill_flagged",
        common::jstr(&bill, "match_status")?,
        "flagged".to_string(),
    )?;
    expect_err_contains(
        env,
        "payment_proposal_blocked",
        procurement::create_payment_proposal(&env.conn, &ctx, now + 140),
        "no eligible bills",
    )?;
    expect_ok(
        env,
        "approve_bill_variance",
        procurement::approve_bill_variance(&env.conn, &ctx, &bill_id, now + 150),
    )?;
    let proposal = expect_ok(
        env,
        "payment_proposal_ok",
        procurement::create_payment_proposal(&env.conn, &ctx, now + 160),
    )?;
    let proposal_id = json_id(&proposal)?;
    expect_ok(
        env,
        "approve_payment_proposal",
        procurement::approve_payment_proposal(
            &env.conn,
            &ctx,
            &proposal_id,
            &env.ledger,
            now + 170,
        ),
    )?;
    let wsf = expect_ok(
        env,
        "create_wh_sf",
        admin_reports::create_warehouse(&env.conn, &ctx, "WH-SF", "Scenario C FIFO", now),
    )?;
    let wsf_id = json_id(&wsf)?;
    env.set_id("wh_sf", &wsf_id);
    let wsw = expect_ok(
        env,
        "create_wh_sw",
        admin_reports::create_warehouse(&env.conn, &ctx, "WH-SW", "Scenario C WA", now),
    )?;
    let wsw_id = json_id(&wsw)?;
    env.set_id("wh_sw", &wsw_id);
    let c_fifo = expect_ok(
        env,
        "create_c_fifo",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("C-FIFO", "C FIFO", "FIFO", 0, 10_000),
            now,
        ),
    )?;
    let c_fifo_id = json_id(&c_fifo)?;
    let c_wa = expect_ok(
        env,
        "create_c_wa",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("C-WA", "C WA", "WA", 0, 10_000),
            now,
        ),
    )?;
    let c_wa_id = json_id(&c_wa)?;
    env.set_id("item_c_fifo", &c_fifo_id);
    env.set_id("item_c_wa", &c_wa_id);
    expect_ok(
        env,
        "c_fifo_recv_1",
        inventory::receive_stock(
            &env.conn,
            &ctx,
            &recv(&c_fifo_id, &wsf_id, 100_000, 10_000),
            now,
        ),
    )?;
    expect_ok(
        env,
        "c_fifo_recv_2",
        inventory::receive_stock(
            &env.conn,
            &ctx,
            &recv(&c_fifo_id, &wsf_id, 100_000, 20_000),
            now + 10,
        ),
    )?;
    expect_ok(
        env,
        "c_wa_recv_1",
        inventory::receive_stock(
            &env.conn,
            &ctx,
            &recv(&c_wa_id, &wsw_id, 100_000, 10_000),
            now,
        ),
    )?;
    expect_ok(
        env,
        "c_wa_recv_2",
        inventory::receive_stock(
            &env.conn,
            &ctx,
            &recv(&c_wa_id, &wsw_id, 100_000, 20_000),
            now + 10,
        ),
    )?;
    expect_ok(
        env,
        "c_fifo_issue_1",
        inventory::issue_stock(
            &env.conn,
            &ctx,
            &issue(&c_fifo_id, &wsf_id, 100_000, "SCE-C"),
            now + 20,
        ),
    )?;
    expect_ok(
        env,
        "c_fifo_issue_2",
        inventory::issue_stock(
            &env.conn,
            &ctx,
            &issue(&c_fifo_id, &wsf_id, 50_000, "SCE-C"),
            now + 30,
        ),
    )?;
    expect_ok(
        env,
        "c_wa_issue_1",
        inventory::issue_stock(
            &env.conn,
            &ctx,
            &issue(&c_wa_id, &wsw_id, 100_000, "SCE-C"),
            now + 20,
        ),
    )?;
    expect_ok(
        env,
        "c_wa_issue_2",
        inventory::issue_stock(
            &env.conn,
            &ctx,
            &issue(&c_wa_id, &wsw_id, 50_000, "SCE-C"),
            now + 30,
        ),
    )?;
    let fv = expect_ok(
        env,
        "fifo_stock_value",
        inventory::stock_value_minor(&env.conn, "acme", Some(&wsf_id)),
    )?;
    expect_eq(env, "fifo_value_1000000", fv, 1_000_000)?;
    let wv = expect_ok(
        env,
        "wa_stock_value",
        inventory::stock_value_minor(&env.conn, "acme", Some(&wsw_id)),
    )?;
    expect_eq(env, "wa_value_750000", wv, 750_000)?;
    let before = expect_ok(
        env,
        "company_value_before_transfer",
        inventory::stock_value_minor(&env.conn, "acme", None),
    )?;
    expect_ok(
        env,
        "transfer_same_company",
        inventory::transfer_stock(
            &env.conn,
            &ctx,
            &inventory::TransferParams {
                item_id: c_fifo_id.clone(),
                from_wh: wsf_id.clone(),
                to_wh: wh_a,
                qty_kth: 5_000,
            },
            now + 40,
        ),
    )?;
    let after = expect_ok(
        env,
        "company_value_after_transfer",
        inventory::stock_value_minor(&env.conn, "acme", None),
    )?;
    expect_eq(env, "transfer_value_unchanged", after, before)?;
    let oh = expect_ok(
        env,
        "c_fifo_on_hand_total",
        inventory::on_hand_total(&env.conn, "acme", &c_fifo_id),
    )?;
    expect_eq(env, "c_fifo_on_hand_50", oh, 50_000)?;
    Ok(())
}

fn part4(env: &mut Env) -> Result<(), String> {
    let now = ts(2026, 11, 4, 9, 0);
    let ctx = env.admin.clone();
    let wh = env.id("wh_a")?;
    let item_b = expect_ok(
        env,
        "create_item_b",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("D-B", "Component B", "WA", 0, 300),
            now,
        ),
    )?;
    let item_b_id = json_id(&item_b)?;
    let item_a = expect_ok(
        env,
        "create_item_a",
        inventory::create_item(
            &env.conn,
            &ctx,
            &new_item("D-A", "Assembly A", "WA", 0, 600),
            now,
        ),
    )?;
    let item_a_id = json_id(&item_a)?;
    expect_ok(
        env,
        "set_lead_time_a",
        manufacturing::set_item_lead_time(&env.conn, &ctx, &item_a_id, 7, now),
    )?;
    expect_ok(
        env,
        "set_lead_time_b",
        manufacturing::set_item_lead_time(&env.conn, &ctx, &item_b_id, 5, now),
    )?;
    expect_ok(
        env,
        "recv_b_100_at_300",
        inventory::receive_stock(&env.conn, &ctx, &recv(&item_b_id, &wh, 100_000, 300), now),
    )?;
    let oh = expect_ok(
        env,
        "on_hand_b",
        inventory::on_hand(&env.conn, "acme", &item_b_id, &wh),
    )?;
    expect_eq(env, "on_hand_b_100", oh, 100_000)?;
    let bom = expect_ok(
        env,
        "create_bom",
        manufacturing::create_bom(
            &env.conn,
            &ctx,
            &item_a_id,
            0,
            &[manufacturing::BomLineIn {
                component_id: item_b_id.clone(),
                qty_kth: 2_000,
                scrap_kth: 0,
            }],
            now + 10,
        ),
    )?;
    let bom_id = json_id(&bom)?;
    expect_ok(
        env,
        "release_bom",
        manufacturing::release_bom(&env.conn, &ctx, &bom_id, now + 20),
    )?;
    let exploded = expect_ok(
        env,
        "explode_bom",
        manufacturing::explode_bom(&env.conn, "acme", &item_a_id, 10_000),
    )?;
    expect_eq(env, "explode_bom_len", exploded.len(), 1)?;
    let first_level = expect_ok(
        env,
        "first_level_requirements",
        manufacturing::first_level_requirements(&env.conn, "acme", &bom_id, 10_000),
    )?;
    expect_eq(env, "first_level_len", first_level.len(), 1)?;
    let req_b = first_level.first().map(|t| t.1).unwrap_or(-1);
    expect_eq(env, "first_level_b_20000", req_b, 20_000)?;
    let wo = expect_ok(
        env,
        "create_work_order",
        manufacturing::create_work_order(&env.conn, &ctx, &item_a_id, 10_000, &wh, now + 30),
    )?;
    let wo_id = json_id(&wo)?;
    expect_ok(
        env,
        "release_work_order",
        manufacturing::release_work_order(&env.conn, &ctx, &wo_id, now + 40),
    )?;
    expect_ok(
        env,
        "issue_wo_materials",
        manufacturing::issue_wo_materials(&env.conn, &ctx, &wo_id, &env.ledger, "USD", now + 50),
    )?;
    let oh = expect_ok(
        env,
        "on_hand_b_after_issue",
        inventory::on_hand(&env.conn, "acme", &item_b_id, &wh),
    )?;
    expect_eq(env, "on_hand_b_80", oh, 80_000)?;
    expect_ok(
        env,
        "report_output",
        manufacturing::report_output(
            &env.conn,
            &ctx,
            &wo_id,
            10_000,
            &env.ledger,
            "USD",
            now + 60,
        ),
    )?;
    let oh = expect_ok(
        env,
        "on_hand_a_after_output",
        inventory::on_hand(&env.conn, "acme", &item_a_id, &wh),
    )?;
    expect_eq(env, "on_hand_a_10", oh, 10_000)?;
    let closed = expect_ok(
        env,
        "close_work_order",
        manufacturing::close_work_order(&env.conn, &ctx, &wo_id, &env.ledger, "USD", now + 70),
    )?;
    expect_eq(
        env,
        "wo_variance_zero",
        common::jint(&closed, "variance_minor")?,
        0,
    )?;
    let customer = env.id("customer")?;
    expect_ok(
        env,
        "mrp_demand_order",
        sales::create_sales_order(
            &env.conn,
            &ctx,
            &customer,
            &[sales::OrderLineIn {
                item_id: item_a_id.clone(),
                qty_kth: 30_000,
                unit_price_minor: 20_000,
                tax_bps: 0,
            }],
            None,
            now + 80,
        ),
    )?;
    let mrp = expect_ok(
        env,
        "mrp_run",
        manufacturing::mrp_run(&env.conn, &ctx, now + 90),
    )?;
    let mrp_s = mrp.to_string();
    expect_eq(
        env,
        "mrp_proposes_work_order",
        mrp_s.contains("work_order"),
        true,
    )?;
    expect_eq(env, "mrp_qty_20000", mrp_s.contains("20000"), true)?;
    let insp = expect_ok(
        env,
        "create_inspection_fail",
        manufacturing::create_inspection(
            &env.conn,
            &ctx,
            "receipt",
            "QA-1",
            &item_b_id,
            1_000,
            "fail",
            5_000,
            &wh,
            Some("damaged"),
            now + 100,
        ),
    )?;
    let insp_id = json_id(&insp)?;
    let oh = expect_ok(
        env,
        "on_hand_b_with_hold",
        inventory::on_hand(&env.conn, "acme", &item_b_id, &wh),
    )?;
    expect_eq(env, "on_hand_b_80_with_hold", oh, 80_000)?;
    expect_ok(
        env,
        "release_quality_hold",
        manufacturing::release_quality_hold(&env.conn, &ctx, &insp_id, now + 110),
    )?;
    expect_ok(
        env,
        "create_inspection_pass",
        manufacturing::create_inspection(
            &env.conn,
            &ctx,
            "receipt",
            "QA-2",
            &item_b_id,
            1_000,
            "pass",
            0,
            &wh,
            None,
            now + 120,
        ),
    )?;
    let dept = expect_ok(
        env,
        "create_department",
        hr::create_department(&env.conn, &ctx, "ENG", "Engineering", now),
    )?;
    let dept_id = json_id(&dept)?;
    let pos = expect_ok(
        env,
        "create_position",
        hr::create_position(&env.conn, &ctx, "ENG-IC", "Engineer", &dept_id, now),
    )?;
    let pos_id = json_id(&pos)?;
    let e1 = expect_ok(
        env,
        "create_emp_1",
        hr::create_employee(
            &env.conn,
            &ctx,
            "E-001",
            "Ada",
            "Lovelace",
            &dept_id,
            Some(&pos_id),
            "2026-03-16",
            310_000,
            "USD",
            now,
        ),
    )?;
    env.set_id("emp_1", &json_id(&e1)?);
    let e2 = expect_ok(
        env,
        "create_emp_2",
        hr::create_employee(
            &env.conn,
            &ctx,
            "E-002",
            "Grace",
            "Hopper",
            &dept_id,
            Some(&pos_id),
            "2025-06-01",
            300_000,
            "USD",
            now,
        ),
    )?;
    let emp_2_id = json_id(&e2)?;
    env.set_id("emp_2", &emp_2_id);
    let e4 = expect_ok(
        env,
        "create_emp_4",
        hr::create_employee(
            &env.conn,
            &ctx,
            "E-004",
            "Alan",
            "Turing",
            &dept_id,
            None,
            "2020-03-01",
            260_000,
            "USD",
            now,
        ),
    )?;
    let emp_4_id = json_id(&e4)?;
    expect_ok(
        env,
        "create_payroll_rule",
        hr::create_payroll_rule(
            &env.conn,
            &ctx,
            &hr::CreatePayrollRule {
                code: "IT10".to_string(),
                name: "Income tax 10%".to_string(),
                kind: "percent_of_gross".to_string(),
                rate_kth: Some(10_000),
                amount_minor: None,
                employer_side: false,
                order_idx: 1,
            },
            now,
        ),
    )?;
    expect_ok(
        env,
        "create_employer_rule",
        hr::create_payroll_rule(
            &env.conn,
            &ctx,
            &hr::CreatePayrollRule {
                code: "ER5".to_string(),
                name: "Employer contribution 5%".to_string(),
                kind: "percent_of_gross".to_string(),
                rate_kth: Some(5_000),
                amount_minor: None,
                employer_side: true,
                order_idx: 2,
            },
            now,
        ),
    )?;
    let pay = expect_ok(
        env,
        "run_payroll_march",
        hr::run_payroll(
            &env.conn,
            &ctx,
            "2026-03",
            &env.ledger,
            Some("PAYROLL-2026-03"),
            now + 200,
        ),
    )?;
    let pay2 = expect_ok(
        env,
        "payroll_idem_replay",
        hr::run_payroll(
            &env.conn,
            &ctx,
            "2026-03",
            &env.ledger,
            Some("PAYROLL-2026-03"),
            now + 210,
        ),
    )?;
    expect_eq(env, "payroll_idem_same", pay2.to_string(), pay.to_string())?;
    let summary = expect_ok(
        env,
        "payroll_summary",
        hr::payroll_summary(&env.conn, "acme", "2026-03"),
    )?;
    // E1: 16/31 days of 3,100.00 = 1,599.60; E2: 3,000.00; E4: 2,600.00; deductions are 10% each.
    expect_eq(
        env,
        "payroll_employees_3",
        common::jint(&summary, "employees")?,
        3,
    )?;
    expect_eq(
        env,
        "payroll_gross_719960",
        common::jint(&summary, "gross_minor")?,
        719_960,
    )?;
    expect_eq(
        env,
        "payroll_deductions_71996",
        common::jint(&summary, "deductions_minor")?,
        71_996,
    )?;
    expect_eq(
        env,
        "payroll_net_647964",
        common::jint(&summary, "net_minor")?,
        647_964,
    )?;
    let lt = expect_ok(
        env,
        "create_leave_type",
        hr::create_leave_type(&env.conn, &ctx, "AL", "Annual leave", 2_000, now),
    )?;
    let lt_id = json_id(&lt)?;
    expect_ok(
        env,
        "accrue_2025_12",
        hr::accrue_leave(&env.conn, &ctx, "2025-12", now),
    )?;
    expect_ok(
        env,
        "accrue_2026_01",
        hr::accrue_leave(&env.conn, &ctx, "2026-01", now),
    )?;
    expect_ok(
        env,
        "accrue_2026_01_again",
        hr::accrue_leave(&env.conn, &ctx, "2026-01", now),
    )?;
    expect_ok(
        env,
        "take_leave_4_days",
        hr::take_leave(
            &env.conn,
            &ctx,
            &emp_2_id,
            &lt_id,
            4_000,
            "2026-02-15",
            now + 10,
        ),
    )?;
    expect_err(
        env,
        "take_leave_over_balance",
        hr::take_leave(
            &env.conn,
            &ctx,
            &emp_2_id,
            &lt_id,
            1_000,
            "2026-02-20",
            now + 20,
        ),
    )?;
    let settle = expect_ok(
        env,
        "final_settlement",
        hr::final_settlement(
            &env.conn,
            &ctx,
            &emp_4_id,
            "2026-11-27",
            0,
            &env.ledger,
            "USD",
            now + 300,
        ),
    )?;
    let settle_s = settle.to_string();
    expect_eq(env, "gratuity_900000", settle_s.contains("900000"), true)?;
    expect_ok(
        env,
        "mark_attendance",
        hr::mark_attendance(
            &env.conn,
            &ctx,
            &emp_2_id,
            "2026-03-02",
            true,
            8_000,
            now + 10,
        ),
    )?;
    Ok(())
}

fn part5(env: &mut Env) -> Result<(), String> {
    let now = ts(2026, 11, 5, 9, 0);
    let ctx = env.admin.clone();
    let emp_2 = env.id("emp_2")?;
    let customer = env.id("customer")?;
    let proj = expect_ok(
        env,
        "create_project",
        projects_service::create_project(
            &env.conn,
            &ctx,
            "P-001",
            "Website rebuild",
            Some(&customer),
            true,
            10_000,
            100_000,
            now,
        ),
    )?;
    let proj_id = json_id(&proj)?;
    env.set_id("project", &proj_id);
    let task = expect_ok(
        env,
        "create_task",
        projects_service::create_task(&env.conn, &ctx, &proj_id, "Design phase", now),
    )?;
    let task_id = json_id(&task)?;
    let ts_log = expect_ok(
        env,
        "log_time",
        projects_service::log_time(
            &env.conn,
            &ctx,
            &proj_id,
            Some(&task_id),
            &emp_2,
            "2026-11-05",
            2_000,
            true,
            now + 10,
        ),
    )?;
    let ts_id = json_id(&ts_log)?;
    expect_ok(
        env,
        "approve_timesheet",
        projects_service::approve_timesheet(&env.conn, &ctx, &ts_id, now + 20),
    )?;
    let inv_ts = expect_ok(
        env,
        "invoice_timesheets",
        projects_service::invoice_timesheets(&env.conn, &ctx, &proj_id, &env.ledger, now + 30),
    )?;
    expect_eq(
        env,
        "timesheets_invoiced_20000",
        inv_ts.to_string().contains("20000"),
        true,
    )?;
    let milestone = expect_ok(
        env,
        "add_milestone",
        projects_service::add_milestone(
            &env.conn,
            &ctx,
            &proj_id,
            "M1 Design sign-off",
            50_000,
            now + 40,
        ),
    )?;
    let ms_id = json_id(&milestone)?;
    expect_ok(
        env,
        "achieve_milestone",
        projects_service::achieve_milestone(&env.conn, &ctx, &ms_id, now + 50),
    )?;
    expect_ok(
        env,
        "invoice_milestone",
        projects_service::invoice_milestone(&env.conn, &ctx, &ms_id, &env.ledger, now + 60),
    )?;
    let pstatus = expect_ok(
        env,
        "project_status",
        projects_service::project_status(&env.conn, "acme", &proj_id),
    )?;
    expect_eq(
        env,
        "project_hours_logged",
        common::jint(&pstatus, "hours_logged_kth")?,
        2_000,
    )?;
    expect_eq(
        env,
        "project_hours_budget",
        common::jint(&pstatus, "hours_budget_kth")?,
        100_000,
    )?;
    expect_eq(
        env,
        "project_billed_70000",
        common::jint(&pstatus, "billed_minor")?,
        70_000,
    )?;
    expect_eq(
        env,
        "project_unbilled_zero",
        common::jint(&pstatus, "unbilled_minor")?,
        0,
    )?;
    let policy = expect_ok(
        env,
        "create_sla_policy",
        projects_service::create_sla_policy(&env.conn, &ctx, "GOLD", 60, 480, 240, now),
    )?;
    let policy_id = json_id(&policy)?;
    expect_ok(
        env,
        "override_xmas_not_working",
        projects_service::set_calendar_day(&env.conn, &ctx, "2026-12-25", false, now),
    )?;
    let m25 = expect_ok(
        env,
        "business_minutes_xmas",
        projects_service::business_minutes_between(
            &env.conn,
            "acme",
            ts(2026, 12, 25, 9, 0),
            ts(2026, 12, 25, 17, 0),
        ),
    )?;
    expect_eq(env, "xmas_zero_minutes", m25, 0)?;
    expect_ok(
        env,
        "restore_xmas_working",
        projects_service::set_calendar_day(&env.conn, &ctx, "2026-12-25", true, now + 10),
    )?;
    let m25b = expect_ok(
        env,
        "business_minutes_xmas_restored",
        projects_service::business_minutes_between(
            &env.conn,
            "acme",
            ts(2026, 12, 25, 9, 0),
            ts(2026, 12, 25, 17, 0),
        ),
    )?;
    expect_eq(env, "xmas_480_minutes", m25b, 480)?;
    let friday_1600 = ts(2026, 11, 27, 16, 0);
    let ticket = expect_ok(
        env,
        "create_ticket",
        projects_service::create_ticket(
            &env.conn,
            &ctx,
            "Printer down",
            Some(&customer),
            "high",
            &policy_id,
            friday_1600,
        ),
    )?;
    let ticket_id = json_id(&ticket)?;
    expect_ok(
        env,
        "pause_ticket",
        projects_service::pause_ticket(&env.conn, &ctx, &ticket_id, ts(2026, 11, 27, 16, 30)),
    )?;
    expect_ok(
        env,
        "resume_ticket",
        projects_service::resume_ticket(&env.conn, &ctx, &ticket_id, ts(2026, 11, 30, 9, 0)),
    )?;
    let sla1 = expect_ok(
        env,
        "check_sla_mon_1500",
        projects_service::check_sla(&env.conn, &ctx, &ticket_id, ts(2026, 11, 30, 15, 0)),
    )?;
    let sla1s = sla1.to_string();
    expect_eq(env, "sla_elapsed_390", sla1s.contains("390"), true)?;
    expect_eq(
        env,
        "sla_escalated_true",
        sla1s.contains("\"escalated\":true"),
        true,
    )?;
    expect_eq(
        env,
        "sla_breached_false",
        sla1s.contains("\"breached\":false"),
        true,
    )?;
    let sla2 = expect_ok(
        env,
        "check_sla_mon_1700",
        projects_service::check_sla(&env.conn, &ctx, &ticket_id, ts(2026, 11, 30, 17, 0)),
    )?;
    let sla2s = sla2.to_string();
    expect_eq(env, "sla_elapsed_510", sla2s.contains("510"), true)?;
    expect_eq(
        env,
        "sla_breached_true",
        sla2s.contains("\"breached\":true"),
        true,
    )?;
    let sla3 = expect_ok(
        env,
        "check_sla_later",
        projects_service::check_sla(&env.conn, &ctx, &ticket_id, ts(2026, 12, 1, 10, 0)),
    )?;
    let sla3s = sla3.to_string();
    expect_eq(
        env,
        "sla_breached_false_later",
        sla3s.contains("\"breached\":false"),
        true,
    )?;
    expect_eq(
        env,
        "sla_already_breached",
        sla3s.contains("\"already_breached\":true"),
        true,
    )?;
    expect_ok(
        env,
        "resolve_ticket",
        projects_service::resolve_ticket(&env.conn, &ctx, &ticket_id, ts(2026, 12, 1, 12, 0)),
    )?;
    expect_ok(
        env,
        "link_knowledge",
        projects_service::link_knowledge(&env.conn, &ctx, &ticket_id, "KB-123", now + 70),
    )?;
    expect_ok(
        env,
        "record_csat",
        projects_service::record_csat(&env.conn, &ctx, &ticket_id, 5, now + 80),
    )?;
    expect_ok(
        env,
        "sla_compliance",
        projects_service::sla_compliance(&env.conn, "acme"),
    )?;
    let gpath = std::env::temp_dir()
        .join(format!("mouli_selftest_g_{}.db", common::new_id()))
        .to_string_lossy()
        .into_owned();
    let gconn = expect_ok(env, "g_open_db", common::open_db(&gpath))?;
    expect_ok(env, "g_schema", crate::schema::create_schema(&gconn))?;
    expect_ok(
        env,
        "g_bootstrap",
        admin_reports::bootstrap_tenant(&gconn, "gco", "gadmin", now),
    )?;
    let gadmin = Ctx {
        tenant: "gco".to_string(),
        user: "gadmin".to_string(),
        roles: vec!["admin".to_string()],
        company: None,
    };
    let gwh = expect_ok(
        env,
        "g_create_warehouse",
        admin_reports::create_warehouse(&gconn, &gadmin, "GW", "G warehouse", now),
    )?;
    let gwh_id = json_id(&gwh)?;
    let gitem = expect_ok(
        env,
        "g_create_item",
        inventory::create_item(
            &gconn,
            &gadmin,
            &new_item("G-1", "Contended item", "FIFO", 0, 5_000),
            now,
        ),
    )?;
    let gitem_id = json_id(&gitem)?;
    expect_ok(
        env,
        "g_receive_10",
        inventory::receive_stock(
            &gconn,
            &gadmin,
            &recv(&gitem_id, &gwh_id, 10_000, 5_000),
            now,
        ),
    )?;
    drop(gconn);
    // Every thread uses its OWN connection to the same file, as the HTTP server does.
    let mut handles = Vec::new();
    for _ in 0..20 {
        let ctxg = gadmin.clone();
        let item = gitem_id.clone();
        let wh2 = gwh_id.clone();
        let path = gpath.clone();
        handles.push(std::thread::spawn(move || -> bool {
            let Ok(c) = common::open_db(&path) else {
                return false;
            };
            let _ = c.busy_timeout(std::time::Duration::from_secs(30));
            inventory::issue_stock(&c, &ctxg, &issue(&item, &wh2, 1_000, "SCEN-G"), now).is_ok()
        }));
    }
    let mut successes = 0i64;
    for h in handles {
        if let Ok(true) = h.join() {
            successes += 1;
        }
    }
    expect_eq(env, "g_exactly_ten_succeed", successes, 10)?;
    let check = expect_ok(env, "g_reopen", common::open_db(&gpath))?;
    let remaining = inventory::on_hand_total(&check, "gco", &gitem_id)?;
    expect_eq(env, "g_on_hand_zero", remaining, 0)?;
    drop(check);
    expect_ok(
        env,
        "g_cleanup",
        std::fs::remove_file(&gpath).map_err(common::e),
    )?;
    Ok(())
}

fn part6(env: &mut Env) -> Result<(), String> {
    let now = ts(2026, 11, 6, 9, 0);
    let ctx = env.admin.clone();
    let req = server::HttpRequest {
        method: "GET".to_string(),
        target: "/health".to_string(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let resp = server::handle_request(&env.conn, &env.ledger, &env.clock, &req);
    expect_eq(env, "health_no_permissions_200", resp.status, 200)?;
    let bad_route = server::HttpRequest {
        target: "/definitely-not-a-route".to_string(),
        headers: vec![
            ("X-Tenant".to_string(), "acme".to_string()),
            ("X-User".to_string(), "admin".to_string()),
            ("X-Roles".to_string(), "admin".to_string()),
        ],
        ..req.clone()
    };
    let resp2 = server::handle_request(&env.conn, &env.ledger, &env.clock, &bad_route);
    let body2 = String::from_utf8_lossy(&resp2.body).into_owned();
    expect_eq(env, "unknown_route_404", resp2.status, 404)?;
    expect_eq(
        env,
        "unknown_route_not_found",
        body2.contains("not found:"),
        true,
    )?;
    let post_health = server::HttpRequest {
        method: "POST".to_string(),
        headers: bad_route.headers.clone(),
        ..req.clone()
    };
    let resp3 = server::handle_request(&env.conn, &env.ledger, &env.clock, &post_health);
    expect_eq(env, "wrong_method_405", resp3.status, 405)?;
    let body3 = String::from_utf8_lossy(&resp3.body).into_owned();
    expect_eq(
        env,
        "wrong_method_message",
        body3.contains("method not allowed"),
        true,
    )?;
    expect_eq(
        env,
        "status_for_404",
        api::status_for("not found: widget"),
        404,
    )?;
    expect_eq(
        env,
        "status_for_405",
        api::status_for("method not allowed"),
        405,
    )?;
    expect_eq(
        env,
        "status_for_403",
        api::status_for("permission denied: nope"),
        403,
    )?;
    expect_eq(
        env,
        "status_for_409",
        api::status_for("already exists: nope"),
        409,
    )?;
    expect_eq(
        env,
        "status_for_400",
        api::status_for("bad input: nope"),
        400,
    )?;
    let q = api::parse_query("q=%20&plus=a+b&n=123");
    expect_eq(
        env,
        "query_pct20",
        q.get("q").and_then(Value::as_str).unwrap_or("").to_string(),
        " ".to_string(),
    )?;
    expect_eq(
        env,
        "query_plus",
        q.get("plus")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "a b".to_string(),
    )?;
    let n = q.get("n").cloned().unwrap_or(Value::Null);
    expect_eq(
        env,
        "query_digits",
        n.to_string().trim_matches('"').to_string(),
        "123".to_string(),
    )?;
    let audit_rows = expect_ok(env, "verify_audit", common::verify_audit(&env.conn, "acme"))?;
    expect_eq(env, "audit_rows_positive", audit_rows >= 1, true)?;
    let entries = entries_snapshot(env);
    expect_eq(env, "ledger_entries_recorded", entries.len() >= 10, true)?;
    let mut balanced = true;
    for e in &entries {
        if common::validate_journal(e).is_err() {
            balanced = false;
        }
    }
    expect_eq(env, "ledger_all_entries_balanced", balanced, true)?;
    expect_eq(
        env,
        "invoice_journal_1100_4000_2100",
        has_entry_with_accounts(&entries, &["1100", "4000", "2100"]),
        true,
    )?;
    expect_eq(
        env,
        "receipt_journal_1000_1100",
        has_entry_with_accounts(&entries, &["1000", "1100"]),
        true,
    )?;
    expect_eq(
        env,
        "goods_receipt_journal_1300_2050",
        has_entry_with_accounts(&entries, &["1300", "2050"]),
        true,
    )?;
    expect_eq(
        env,
        "bill_journal_2050_5100_2000",
        has_entry_with_accounts(&entries, &["2050", "5100", "2000"]),
        true,
    )?;
    let all_accounts: std::collections::HashSet<String> = entries
        .iter()
        .flat_map(|e| e.lines.iter().map(|l| l.account.clone()))
        .collect();
    expect_eq(
        env,
        "payroll_account_6000",
        all_accounts.contains("6000"),
        true,
    )?;
    expect_eq(
        env,
        "payroll_account_6100",
        all_accounts.contains("6100"),
        true,
    )?;
    expect_eq(
        env,
        "payroll_account_2200",
        all_accounts.contains("2200"),
        true,
    )?;
    expect_eq(
        env,
        "payroll_account_2300",
        all_accounts.contains("2300"),
        true,
    )?;
    expect_ok(
        env,
        "tenant2_bootstrap",
        admin_reports::bootstrap_tenant(&env.conn, "other", "boss", now),
    )?;
    let other = other_ctx();
    let other_whs = expect_ok(
        env,
        "tenant2_warehouses",
        admin_reports::list_warehouses(&env.conn, "other"),
    )?;
    expect_eq(env, "tenant2_sees_no_warehouses", other_whs.len(), 0)?;
    let (ia, wha, order) = (env.id("item_a")?, env.id("wh_a")?, env.id("order")?);
    expect_err(
        env,
        "tenant2_cannot_issue",
        inventory::issue_stock(&env.conn, &other, &issue(&ia, &wha, 1_000, "iso"), now),
    )?;
    expect_err(
        env,
        "tenant2_cannot_receive",
        inventory::receive_stock(&env.conn, &other, &recv(&ia, &wha, 1_000, 100), now),
    )?;
    expect_err(
        env,
        "tenant2_cannot_invoice",
        sales::create_invoice_from_order(&env.conn, &other, &order, &env.ledger, None, now),
    )?;
    expect_err(
        env,
        "tenant2_cannot_deliver",
        sales::create_delivery(
            &env.conn,
            &other,
            &order,
            &wha,
            &[sales::DeliveryLineIn {
                order_line_no: 1,
                qty_kth: 100,
            }],
            None,
            now,
        ),
    )?;
    let other_notifs = expect_ok(
        env,
        "tenant2_notifications",
        admin_reports::list_notifications(&env.conn, "other", "boss", false, 10),
    )?;
    expect_eq(env, "tenant2_sees_no_notifications", other_notifs.len(), 0)?;
    let unread = expect_ok(
        env,
        "pm_notifications_unread",
        admin_reports::list_notifications(&env.conn, "acme", "procurement-manager", true, 10),
    )?;
    let unread_before = unread.len();
    expect_eq(env, "pm_notification_present", unread_before >= 1, true)?;
    if let Some(first) = unread.first() {
        let nid = common::jstr(first, "id")?;
        expect_err_contains(
            env,
            "other_user_cannot_mark_read",
            admin_reports::mark_notification_read(&env.conn, &ctx, &nid, now),
            "different user",
        )?;
        let pm = Ctx {
            tenant: "acme".to_string(),
            user: "procurement-manager".to_string(),
            roles: vec!["procurement-manager".to_string()],
            company: None,
        };
        expect_err_contains(
            env,
            "pm_needs_notifications_read",
            admin_reports::mark_notification_read(&env.conn, &pm, &nid, now),
            "notifications.read",
        )?;
        expect_ok(
            env,
            "grant_pm_notifications_read",
            admin_reports::grant_permission(
                &env.conn,
                &ctx,
                "procurement-manager",
                "notifications.read",
                now,
            ),
        )?;
        expect_ok(
            env,
            "mark_notification_read",
            admin_reports::mark_notification_read(&env.conn, &pm, &nid, now),
        )?;
    } else {
        return Err(
            "check 'mark_notification_read' failed: no unread notification found".to_string(),
        );
    }
    let unread_after = expect_ok(
        env,
        "pm_notifications_after",
        admin_reports::list_notifications(&env.conn, "acme", "procurement-manager", true, 10),
    )?;
    expect_eq(
        env,
        "pm_notification_marked_read",
        unread_after.len(),
        unread_before - 1,
    )?;
    let stock_report = admin_reports::run_report(&env.conn, &ctx, "stock", &json!({}));
    expect_eq(env, "report_stock_runs", stock_report.is_ok(), true)?;
    let payroll_report =
        admin_reports::run_report(&env.conn, &ctx, "payroll", &json!({ "period": "2026-03" }));
    expect_eq(env, "report_payroll_runs", payroll_report.is_ok(), true)?;
    expect_err(
        env,
        "report_unknown_name",
        admin_reports::run_report(&env.conn, &ctx, "definitely_not_a_report", &json!({})),
    )?;
    let tampered = env
        .conn
        .execute(
            "UPDATE audit_log SET payload = '{}' WHERE tenant_id = 'acme' AND seq = 1",
            [],
        )
        .map_err(common::e)?;
    expect_eq(env, "audit_tamper_row_updated", tampered, 1)?;
    expect_err_contains(
        env,
        "audit_tamper_detected",
        common::verify_audit(&env.conn, "acme"),
        "seq",
    )?;
    Ok(())
}

/// Runs every check against a fresh in-memory database; the error names the first failure.
pub fn run_selftest() -> Result<serde_json::Value, String> {
    let conn = common::open_memory()?;
    crate::schema::create_schema(&conn)?;
    let now = ts(2026, 3, 1, 9, 0);
    admin_reports::bootstrap_tenant(&conn, "acme", "admin", now)?;
    let admin = Ctx {
        tenant: "acme".to_string(),
        user: "admin".to_string(),
        roles: vec!["admin".to_string()],
        company: None,
    };
    let mut env = Env {
        conn,
        ledger: TestLedger {
            entries: Mutex::new(Vec::new()),
        },
        clock: FakeClock::new(now),
        admin,
        ids: std::collections::HashMap::new(),
        results: Vec::new(),
    };
    part1(&mut env)?;
    part2(&mut env)?;
    part3(&mut env)?;
    part4(&mut env)?;
    part5(&mut env)?;
    part6(&mut env)?;
    let count = env.results.len();
    if count < 150 {
        return Err(format!(
            "selftest produced only {count} checks (expected at least 150)"
        ));
    }
    let mut checks = serde_json::Map::new();
    for (name, res) in &env.results {
        checks.insert(name.clone(), Value::String(res.clone()));
    }
    Ok(json!({ "status": "PASS", "count": count, "checks": Value::Object(checks) }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn selftest_passes() {
        let v = super::run_selftest().unwrap();
        assert_eq!(v["status"], "PASS");
    }
}
