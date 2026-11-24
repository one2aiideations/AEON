//! The REST router and the admin/report helpers, driven the way a client would.

use aeon_ops::admin_reports;
use aeon_ops::api::{parse_query, status_for, Api};
use aeon_ops::common::{self, Ctx, FakeClock, TestLedger};
use aeon_ops::schema;
use rusqlite::Connection;
use serde_json::{json, Value};

const T: &str = "t1";

fn setup() -> Connection {
    let c = common::open_memory().unwrap();
    schema::create_schema(&c).unwrap();
    admin_reports::bootstrap_tenant(&c, T, "alice", 1).unwrap();
    c
}

fn admin() -> Ctx {
    Ctx::new(T, "alice", &["admin"], Some("ACME"))
}

fn call(api: &Api, m: &str, p: &str, body: Value) -> Result<Value, String> {
    api.handle(m, p, &body, &admin())
}

#[test]
fn health_needs_no_database_or_permission() {
    let c = setup();
    let (l, k) = (TestLedger::new(), FakeClock::new(1_000));
    let api = Api::new(&c, &l, &k);
    let nobody = Ctx::new("nobody", "x", &[], None);
    assert_eq!(
        api.handle("get", "/health", &json!({}), &nobody).unwrap()["status"],
        "ok"
    );
    assert!(api
        .handle("POST", "/health", &json!({}), &nobody)
        .unwrap_err()
        .contains("method not allowed"));
}

#[test]
fn routing_errors_map_to_http_statuses() {
    let c = setup();
    let (l, k) = (TestLedger::new(), FakeClock::new(1_000));
    let api = Api::new(&c, &l, &k);
    let nf = call(&api, "GET", "/nope", json!({})).unwrap_err();
    assert!(nf.starts_with("not found: GET /nope"));
    assert_eq!(status_for(&nf), 404);
    let bad = call(&api, "DELETE", "/admin/warehouses", json!({})).unwrap_err();
    assert_eq!(status_for(&bad), 405, "{bad}");
    let missing = call(&api, "POST", "/admin/warehouses", json!({"code": "W1"})).unwrap_err();
    assert!(missing.contains("name"), "{missing}");
    assert_eq!(status_for(&missing), 400);
    let denied = Api::new(&c, &l, &k)
        .handle(
            "POST",
            "/admin/warehouses",
            &json!({"code": "W2", "name": "Two"}),
            &Ctx::new(T, "bob", &["nobody"], None),
        )
        .unwrap_err();
    assert_eq!(status_for(&denied), 403);
    call(
        &api,
        "POST",
        "/admin/warehouses",
        json!({"code": "W1", "name": "Main"}),
    )
    .unwrap();
    let dup = call(
        &api,
        "POST",
        "/admin/warehouses",
        json!({"code": "W1", "name": "Again"}),
    )
    .unwrap_err();
    assert_eq!(status_for(&dup), 409, "{dup}");
}

#[test]
fn query_strings_are_decoded_and_the_body_wins() {
    let q = parse_query("a=1&b=-7&c=hello%20world&d=x+y&e=&f=12ab&g=%zz&h");
    assert_eq!(q["a"], 1);
    assert_eq!(q["b"], -7);
    assert_eq!(q["c"], "hello world");
    assert_eq!(q["d"], "x y");
    assert_eq!(q["e"], "");
    assert_eq!(q["f"], "12ab");
    assert_eq!(q["g"], "%zz", "a bad escape is kept, not guessed");
    assert_eq!(q["h"], "");
    let c = setup();
    let (l, k) = (TestLedger::new(), FakeClock::new(1_000));
    let api = Api::new(&c, &l, &k);
    call(
        &api,
        "POST",
        "/admin/warehouses",
        json!({"code": "W1", "name": "Main"}),
    )
    .unwrap();
    let rows = call(&api, "GET", "/admin/warehouses?code=ignored", json!({})).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
}

#[test]
fn floats_are_refused_for_money_and_quantities() {
    let c = setup();
    let (l, k) = (TestLedger::new(), FakeClock::new(1_000));
    let api = Api::new(&c, &l, &k);
    let err = call(&api, "POST", "/inventory/items", json!({"code": "A", "name": "A", "uom": "EA", "valuation_method": "FIFO", "initial_cost_minor": 12.5})).unwrap_err();
    assert!(err.contains("initial_cost_minor"), "{err}");
}

#[test]
fn quote_to_cash_through_the_api_then_reports() {
    let c = setup();
    let ledger = TestLedger::new();
    let clock = FakeClock::new(1_800_000_000);
    let api = Api::new(&c, &ledger, &clock);
    let wh = call(
        &api,
        "POST",
        "/admin/warehouses",
        json!({"code": "W1", "name": "Main"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let item = call(&api, "POST", "/inventory/items", json!({"code": "WIDGET", "name": "Widget", "uom": "EA", "valuation_method": "FIFO", "initial_cost_minor": 0})).unwrap()["id"].as_str().unwrap().to_string();
    call(
        &api,
        "POST",
        "/inventory/receive",
        json!({"item_id": item, "warehouse_id": wh, "qty_kth": 10_000, "unit_cost_minor": 5000}),
    )
    .unwrap();
    let cust = call(
        &api,
        "POST",
        "/sales/customers",
        json!({"code": "C1", "name": "Acme", "currency": "USD"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let order = call(&api, "POST", "/sales/orders", json!({"customer_id": cust, "idempotency_key": "k1", "lines": [{"item_id": item, "qty_kth": 4_000, "unit_price_minor": 10_000, "tax_bps": 1000}]})).unwrap();
    let again = call(&api, "POST", "/sales/orders", json!({"customer_id": cust, "idempotency_key": "k1", "lines": [{"item_id": item, "qty_kth": 4_000, "unit_price_minor": 10_000, "tax_bps": 1000}]})).unwrap();
    assert_eq!(order, again);
    assert_eq!(
        order["total_minor"], 44_000,
        "4 units at 100.00 plus 10% tax, the same as the invoice"
    );
    let oid = order["id"].as_str().unwrap().to_string();
    let d = call(&api, "POST", "/sales/deliveries", json!({"order_id": oid, "warehouse_id": wh, "lines": [{"order_line_no": 1, "qty_kth": 4_000}]})).unwrap();
    call(
        &api,
        "POST",
        "/sales/deliveries/post",
        json!({"delivery_id": d["id"]}),
    )
    .unwrap();
    let inv = call(&api, "POST", "/sales/invoices", json!({"order_id": oid})).unwrap();
    assert_eq!(inv["total_minor"], 44_000);
    let status = call(
        &api,
        "GET",
        &format!("/sales/orders/status?order_id={oid}"),
        json!({}),
    )
    .unwrap();
    assert_eq!(status[0]["delivered_kth"], 4_000);
    let stock = call(&api, "GET", "/inventory/stock", json!({})).unwrap();
    assert_eq!(stock[0]["qty_kth"], 6_000);
    let rep = call(
        &api,
        "GET",
        "/reports/sales_by_period?from_ts=0&to_ts=2000000000",
        json!({}),
    )
    .unwrap();
    assert_eq!(rep["invoice_count"], 1);
    assert_eq!(rep["by_currency"][0]["total_minor"], 44_000);
    assert_eq!(rep["by_currency"][0]["net_minor"], 40_000);
    let stock_rep = call(&api, "GET", "/reports/stock", json!({})).unwrap();
    assert_eq!(stock_rep["rows"][0]["code"], "WIDGET");
    let unknown = call(&api, "GET", "/reports/nonsense", json!({})).unwrap_err();
    assert!(unknown.contains("valid reports are"), "{unknown}");
    let audit = call(&api, "GET", "/audit/verify", json!({})).unwrap();
    assert!(audit["verified_rows"].as_i64().unwrap() > 5);
}

#[test]
fn bootstrap_is_one_time_and_roles_can_be_managed() {
    let c = setup();
    assert!(admin_reports::bootstrap_tenant(&c, T, "mallory", 2)
        .unwrap_err()
        .contains("already bootstrapped"));
    let (l, k) = (TestLedger::new(), FakeClock::new(1_000));
    let api = Api::new(&c, &l, &k);
    call(
        &api,
        "POST",
        "/admin/roles/grant",
        json!({"role": "clerk", "permission": "inventory.receive"}),
    )
    .unwrap();
    call(
        &api,
        "POST",
        "/admin/approval-limits",
        json!({"role": "clerk", "limit_minor": 5_000}),
    )
    .unwrap();
    assert!(call(
        &api,
        "POST",
        "/admin/approval-limits",
        json!({"role": "clerk", "limit_minor": -5})
    )
    .is_err());
    let clerk = Ctx::new(T, "carol", &["clerk"], None);
    assert!(api.handle("POST", "/inventory/items", &json!({"code": "A", "name": "A", "uom": "EA", "valuation_method": "FIFO", "initial_cost_minor": 0}), &clerk).unwrap_err().contains("permission denied"));
    let revoked = call(
        &api,
        "POST",
        "/admin/roles/revoke",
        json!({"role": "clerk", "permission": "inventory.receive"}),
    )
    .unwrap();
    assert_eq!(revoked["removed"], true);
}

#[test]
fn notifications_belong_to_their_user() {
    let c = setup();
    let (l, k) = (TestLedger::new(), FakeClock::new(1_000));
    let api = Api::new(&c, &l, &k);
    let nid = common::notify(&c, T, "alice", "demo", &json!({"x": 1}), 5).unwrap();
    let list = call(
        &api,
        "GET",
        "/notifications?user=alice&unread_only=true",
        json!({}),
    )
    .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let other = Ctx::new(T, "bob", &["admin"], None);
    assert!(api
        .handle(
            "POST",
            "/notifications/read",
            &json!({"notification_id": nid}),
            &other
        )
        .unwrap_err()
        .contains("different user"));
    call(
        &api,
        "POST",
        "/notifications/read",
        json!({"notification_id": nid}),
    )
    .unwrap();
    let after = call(
        &api,
        "GET",
        "/notifications?user=alice&unread_only=true",
        json!({}),
    )
    .unwrap();
    assert!(after.as_array().unwrap().is_empty());
}
