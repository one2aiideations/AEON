//! Demo data: one realistic company loaded into an already-bootstrapped tenant.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::admin_reports;
use crate::common::{jopt_str, jstr, qty_from_int, Clock, Ctx, Ledger, Res};
use crate::hr;
use crate::inventory::{self, CreateItem, ReceiveParams};
use crate::manufacturing;
use crate::procurement::{self, CreateSupplier, GrLineIn, PoLineIn};
use crate::projects_service;
use crate::sales::{self, CreateCustomer, DeliveryLineIn, OrderLineIn};

/// Employee: code, first name, last name, in Operations?, engineer/rep position, hire date, salary.
type EmployeeSpec = (
    &'static str,
    &'static str,
    &'static str,
    bool,
    Option<bool>,
    &'static str,
    i64,
);

fn id_of(v: &Value) -> Res<String> {
    jstr(v, "id").map_err(|_| format!("no id field in value {}", v))
}

/// Load one realistic demo company into an already-bootstrapped tenant.
///
/// Fails with a clear message when the tenant is not bootstrapped or already has demo data.
pub fn load_demo(
    conn: &Connection,
    ledger: &dyn Ledger,
    clock: &dyn Clock,
    tenant: &str,
) -> Res<Value> {
    if tenant.trim().is_empty() {
        return Err("tenant must not be empty".to_string());
    }
    let now = clock.now();
    let ctx = Ctx::new(tenant, "admin", &["admin"], Some("ACME"));

    // A bootstrapped tenant has the admin role; anything else is refused with a clear message.
    admin_reports::run_report(conn, &ctx, "open_orders", &serde_json::json!({})).map_err(
        |err| {
            format!(
                "tenant '{}' is not bootstrapped (or admin has no access): {}",
                tenant, err
            )
        },
    )?;

    for w in admin_reports::list_warehouses(conn, tenant)? {
        if jopt_str(&w, "code").as_deref() == Some("MAIN") {
            return Err(format!(
                "tenant '{}' already has demo data (warehouse MAIN exists); refusing to load twice",
                tenant
            ));
        }
    }

    // ---- warehouses ----
    let wh_main_id = id_of(
        &admin_reports::create_warehouse(conn, &ctx, "MAIN", "Main warehouse", now)
            .map_err(|err| format!("demo: warehouse MAIN: {}", err))?,
    )?;
    let wh_east_id = id_of(
        &admin_reports::create_warehouse(conn, &ctx, "EAST", "East depot", now)
            .map_err(|err| format!("demo: warehouse EAST: {}", err))?,
    )?;

    // ---- customers ----
    let customer_specs = [
        ("CUST-001", "Northwind Traders", 25_000_000i64),
        ("CUST-002", "Bluefin Retail Group", 50_000_000i64),
        ("CUST-003", "Harbor & Co", 15_000_000i64),
    ];
    let mut customer_ids: Vec<String> = Vec::new();
    for (code, name, limit) in customer_specs {
        let v = sales::create_customer(
            conn,
            &ctx,
            &CreateCustomer {
                code: code.to_string(),
                name: name.to_string(),
                currency: "USD".to_string(),
                credit_limit_minor: limit,
            },
            now,
        )
        .map_err(|err| format!("demo: customer {}: {}", code, err))?;
        customer_ids.push(id_of(&v)?);
    }

    // ---- suppliers ----
    let supplier_specs = [
        ("SUP-001", "Consolidated Metals", 30),
        ("SUP-002", "Pacific Polymers", 45),
        ("SUP-003", "Summit Fasteners", 14),
    ];
    let mut supplier_ids: Vec<String> = Vec::new();
    for (code, name, terms) in supplier_specs {
        let v = procurement::create_supplier(
            conn,
            &ctx,
            &CreateSupplier {
                code: code.to_string(),
                name: name.to_string(),
                currency: "USD".to_string(),
                payment_terms_days: terms,
            },
            now,
        )
        .map_err(|err| format!("demo: supplier {}: {}", code, err))?;
        supplier_ids.push(id_of(&v)?);
    }

    // ---- items: (code, name, uom, valuation, unit cost minor, reorder point in thousandths) ----
    let item_specs: [(&str, &str, &str, &str, i64, i64); 20] = [
        (
            "RAW-STEEL",
            "Cold-rolled steel sheet",
            "KG",
            "FIFO",
            1_800,
            50_000,
        ),
        (
            "RAW-ALU",
            "Aluminium extrusion",
            "KG",
            "FIFO",
            2_400,
            40_000,
        ),
        ("RAW-PLAS", "Polymer granulate", "KG", "WA", 1_500, 60_000),
        (
            "RAW-WOOD",
            "Kiln-dried pine board",
            "KG",
            "FIFO",
            900,
            80_000,
        ),
        ("RAW-GLUE", "PVA adhesive", "L", "WA", 3_200, 10_000),
        ("RAW-PAINT", "Water-based paint", "L", "WA", 4_500, 15_000),
        (
            "RAW-PACK",
            "Cardboard box, large",
            "EA",
            "FIFO",
            250,
            200_000,
        ),
        (
            "RAW-WIRE",
            "Copper wiring, 1.5mm",
            "M",
            "FIFO",
            750,
            100_000,
        ),
        ("RAW-CHEM", "Degreasing solvent", "L", "WA", 12_000, 5_000),
        (
            "RAW-COOL",
            "CNC coolant concentrate",
            "L",
            "WA",
            6_800,
            8_000,
        ),
        (
            "RAW-FASTEN",
            "M6 socket-head bolt",
            "EA",
            "FIFO",
            30,
            1_000_000,
        ),
        (
            "TRD-WIDGET",
            "Standard widget",
            "EA",
            "FIFO",
            1_500,
            100_000,
        ),
        ("TRD-GADGET", "Deluxe gadget", "EA", "FIFO", 8_900, 50_000),
        (
            "TRD-SPARE",
            "Replacement gasket set",
            "EA",
            "FIFO",
            5_500,
            40_000,
        ),
        (
            "TRD-TOOLS",
            "Cordless drill kit",
            "EA",
            "FIFO",
            12_500,
            20_000,
        ),
        (
            "TRD-SAFETY",
            "Safety gloves, pair",
            "EA",
            "WA",
            2_100,
            150_000,
        ),
        (
            "TRD-CLEAN",
            "All-purpose cleaner",
            "EA",
            "WA",
            1_750,
            120_000,
        ),
        (
            "SUB-PANEL",
            "Sub-assembly: door panel",
            "EA",
            "FIFO",
            3_400,
            25_000,
        ),
        (
            "FG-CHAIR",
            "Finished: ergonomic chair",
            "EA",
            "FIFO",
            18_000,
            20_000,
        ),
        (
            "FG-DESK",
            "Finished: writing desk",
            "EA",
            "FIFO",
            25_000,
            10_000,
        ),
    ];
    let mut item_ids: HashMap<String, String> = HashMap::new();
    for (code, name, uom, method, cost, reorder) in item_specs {
        let v = inventory::create_item(
            conn,
            &ctx,
            &CreateItem {
                code: code.to_string(),
                name: name.to_string(),
                uom: uom.to_string(),
                valuation_method: method.to_string(),
                reorder_point_kth: reorder,
                allows_negative: false,
                initial_cost_minor: cost,
            },
            now,
        )
        .map_err(|err| format!("demo: item {}: {}", code, err))?;
        let id = id_of(&v)?;
        let qty = qty_from_int(if code.starts_with("RAW") { 500 } else { 100 })?;
        inventory::receive_stock(
            conn,
            &ctx,
            &ReceiveParams {
                item_id: id.clone(),
                warehouse_id: wh_main_id.clone(),
                qty_kth: qty,
                unit_cost_minor: cost,
                batch_no: None,
                batch_expiry: None,
                ref_type: Some("opening".to_string()),
                ref_id: Some("demo".to_string()),
            },
            now,
        )
        .map_err(|err| format!("demo: opening stock for {}: {}", code, err))?;
        item_ids.insert(code.to_string(), id);
    }
    let item = |code: &str| -> Res<String> {
        item_ids
            .get(code)
            .cloned()
            .ok_or_else(|| format!("demo: internal error: missing item {}", code))
    };

    // ---- multi-level BOMs ----
    // SUB-PANEL = 2 RAW-WOOD + 12 RAW-FASTEN
    // FG-DESK   = 1 SUB-PANEL + 8 RAW-WOOD + 0.5 RAW-PAINT (5% scrap on the paint)
    let sub_bom = id_of(
        &manufacturing::create_bom(
            conn,
            &ctx,
            &item("SUB-PANEL")?,
            0,
            &[
                manufacturing::BomLineIn {
                    component_id: item("RAW-WOOD")?,
                    qty_kth: 2_000,
                    scrap_kth: 0,
                },
                manufacturing::BomLineIn {
                    component_id: item("RAW-FASTEN")?,
                    qty_kth: 12_000,
                    scrap_kth: 0,
                },
            ],
            now,
        )
        .map_err(|err| format!("demo: SUB-PANEL bom: {}", err))?,
    )?;
    manufacturing::release_bom(conn, &ctx, &sub_bom, now)
        .map_err(|err| format!("demo: release SUB-PANEL bom: {}", err))?;
    let desk_bom = id_of(
        &manufacturing::create_bom(
            conn,
            &ctx,
            &item("FG-DESK")?,
            0,
            &[
                manufacturing::BomLineIn {
                    component_id: item("SUB-PANEL")?,
                    qty_kth: 1_000,
                    scrap_kth: 0,
                },
                manufacturing::BomLineIn {
                    component_id: item("RAW-WOOD")?,
                    qty_kth: 8_000,
                    scrap_kth: 0,
                },
                manufacturing::BomLineIn {
                    component_id: item("RAW-PAINT")?,
                    qty_kth: 500,
                    scrap_kth: 5_000,
                },
            ],
            now,
        )
        .map_err(|err| format!("demo: FG-DESK bom: {}", err))?,
    )?;
    manufacturing::release_bom(conn, &ctx, &desk_bom, now)
        .map_err(|err| format!("demo: release FG-DESK bom: {}", err))?;

    // ---- departments, positions, employees ----
    let dept_ops_id = id_of(
        &hr::create_department(conn, &ctx, "OPS", "Operations", now)
            .map_err(|err| format!("demo: department OPS: {}", err))?,
    )?;
    let dept_sales_id = id_of(
        &hr::create_department(conn, &ctx, "SALES", "Sales", now)
            .map_err(|err| format!("demo: department SALES: {}", err))?,
    )?;
    let pos_eng_id = id_of(
        &hr::create_position(conn, &ctx, "ENG1", "Assembly Engineer", &dept_ops_id, now)
            .map_err(|err| format!("demo: position ENG1: {}", err))?,
    )?;
    let pos_rep_id = id_of(
        &hr::create_position(conn, &ctx, "REP1", "Account Manager", &dept_sales_id, now)
            .map_err(|err| format!("demo: position REP1: {}", err))?,
    )?;
    let employee_specs: [EmployeeSpec; 5] = [
        (
            "EMP-001",
            "Aisha",
            "Rahman",
            true,
            Some(true),
            "2021-04-12",
            850_000,
        ),
        (
            "EMP-002",
            "Marco",
            "Delacroix",
            true,
            Some(true),
            "2022-09-01",
            720_000,
        ),
        (
            "EMP-003",
            "Priya",
            "Natarajan",
            true,
            None,
            "2023-01-16",
            640_000,
        ),
        (
            "EMP-004",
            "Sofia",
            "Lindqvist",
            false,
            Some(false),
            "2020-11-30",
            780_000,
        ),
        (
            "EMP-005",
            "Tom",
            "Okafor",
            false,
            Some(false),
            "2025-06-02",
            520_000,
        ),
    ];
    let mut employee_ids: Vec<String> = Vec::new();
    for (code, first, last, in_ops, engineer, hire, salary) in employee_specs {
        let dept = if in_ops { &dept_ops_id } else { &dept_sales_id };
        let position = engineer.map(|eng| {
            if eng {
                pos_eng_id.as_str()
            } else {
                pos_rep_id.as_str()
            }
        });
        let v = hr::create_employee(
            conn, &ctx, code, first, last, dept, position, hire, salary, "USD", now,
        )
        .map_err(|err| format!("demo: employee {}: {}", code, err))?;
        employee_ids.push(id_of(&v)?);
    }

    // ---- payroll rules: one deduction, one employer contribution ----
    hr::create_payroll_rule(
        conn,
        &ctx,
        &hr::CreatePayrollRule {
            code: "PF".to_string(),
            name: "Provident fund (employee, 12%)".to_string(),
            kind: "percent_of_basic".to_string(),
            rate_kth: Some(12_000),
            amount_minor: None,
            employer_side: false,
            order_idx: 1,
        },
        now,
    )
    .map_err(|err| format!("demo: payroll rule PF: {}", err))?;
    hr::create_payroll_rule(
        conn,
        &ctx,
        &hr::CreatePayrollRule {
            code: "SI-ER".to_string(),
            name: "Social insurance (employer, 3.25%)".to_string(),
            kind: "percent_of_gross".to_string(),
            rate_kth: Some(3_250),
            amount_minor: None,
            employer_side: true,
            order_idx: 2,
        },
        now,
    )
    .map_err(|err| format!("demo: payroll rule SI-ER: {}", err))?;
    let leave_type_id = id_of(
        &hr::create_leave_type(conn, &ctx, "ANNUAL", "Annual leave", 1_500, now)
            .map_err(|err| format!("demo: leave type: {}", err))?,
    )?;

    // ---- project with a customer, a rate and two tasks ----
    let project_id = id_of(
        &projects_service::create_project(
            conn,
            &ctx,
            "PRJ-001",
            "Warehouse fit-out",
            Some(&customer_ids[0]),
            true,
            15_000,
            1_000_000,
            now,
        )
        .map_err(|err| format!("demo: project: {}", err))?,
    )?;
    for title in ["Site survey and measurements", "Racking installation"] {
        projects_service::create_task(conn, &ctx, &project_id, title, now)
            .map_err(|err| format!("demo: task '{}': {}", title, err))?;
    }

    // ---- SLA policy and an open ticket ----
    let sla_id = id_of(
        &projects_service::create_sla_policy(conn, &ctx, "Gold", 30, 480, 240, now)
            .map_err(|err| format!("demo: SLA policy: {}", err))?,
    )?;
    projects_service::create_ticket(
        conn,
        &ctx,
        "Scanner integration: labels not printing",
        Some(&customer_ids[1]),
        "high",
        &sla_id,
        now,
    )
    .map_err(|err| format!("demo: ticket: {}", err))?;

    // ---- a sales order with a partial delivery ----
    let order = sales::create_sales_order(
        conn,
        &ctx,
        &customer_ids[0],
        &[OrderLineIn {
            item_id: item("FG-CHAIR")?,
            qty_kth: qty_from_int(20)?,
            unit_price_minor: 18_000,
            tax_bps: 1_000,
        }],
        None,
        now,
    )
    .map_err(|err| format!("demo: sales order: {}", err))?;
    let order_id = id_of(&order)?;
    let delivery = sales::create_delivery(
        conn,
        &ctx,
        &order_id,
        &wh_main_id,
        &[DeliveryLineIn {
            order_line_no: 1,
            qty_kth: qty_from_int(8)?,
        }],
        None,
        now,
    )
    .map_err(|err| format!("demo: delivery: {}", err))?;
    sales::post_delivery(conn, &ctx, &id_of(&delivery)?, now)
        .map_err(|err| format!("demo: post delivery: {}", err))?;

    // ---- a purchase order with a partial receipt ----
    let po = procurement::create_po(
        conn,
        &ctx,
        &supplier_ids[0],
        &[PoLineIn {
            item_id: item("RAW-STEEL")?,
            qty_kth: qty_from_int(10)?,
            unit_cost_minor: 1_800,
        }],
        Some("2026-12-31"),
        None,
        now,
    )
    .map_err(|err| format!("demo: purchase order: {}", err))?;
    let po_id = id_of(&po)?;
    procurement::receive_goods(
        conn,
        &ctx,
        &po_id,
        &wh_main_id,
        &[GrLineIn {
            po_line_no: 1,
            qty_kth: qty_from_int(4)?,
            batch_no: None,
            batch_expiry: None,
        }],
        ledger,
        None,
        now,
    )
    .map_err(|err| format!("demo: goods receipt: {}", err))?;

    Ok(json!({
        "tenant": tenant,
        "warehouses": 2,
        "customers": customer_ids.len(),
        "suppliers": supplier_ids.len(),
        "items": item_ids.len(),
        "boms": 2,
        "departments": 2,
        "positions": 2,
        "employees": employee_ids.len(),
        "payroll_rules": 2,
        "leave_types": 1,
        "leave_type_id": leave_type_id,
        "projects": 1,
        "project_id": project_id,
        "tasks": 2,
        "sla_policies": 1,
        "tickets": 1,
        "sales_orders": 1,
        "sales_order_id": order_id,
        "deliveries": 1,
        "purchase_orders": 1,
        "purchase_order_id": po_id,
        "goods_receipts": 1,
        "warehouse_ids": [wh_main_id, wh_east_id],
    }))
}
