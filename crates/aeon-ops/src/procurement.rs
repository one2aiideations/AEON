//! Suppliers, requisitions with approval and escalation, RFQs, purchase orders, goods receipts, three-way bill matching and payment proposals.

use crate::common::*;
use crate::inventory;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

fn today(now: i64) -> String {
    ymd(now).chars().take(10).collect()
}

/// Parameters for creating a supplier.
pub struct CreateSupplier {
    pub code: String,
    pub name: String,
    pub currency: String,
    pub payment_terms_days: i64,
}

/// Creates a supplier. [procurement.suppliers.create]
pub fn create_supplier(conn: &Connection, ctx: &Ctx, p: &CreateSupplier, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.suppliers.create")?;
        if p.code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        if p.name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if p.currency.trim().is_empty() {
            return Err("field 'currency' must not be empty".to_string());
        }
        if p.payment_terms_days < 0 {
            return Err(
                "field 'payment_terms_days' must be greater than or equal to zero".to_string(),
            );
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO suppliers (tenant_id, id, code, name, currency, payment_terms_days) VALUES (?1,?2,?3,?4,?5,?6)",
            params![ctx.tenant, id, p.code, p.name, p.currency, p.payment_terms_days],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "supplier.created",
            &id,
            &json!({"code": p.code}),
            now,
        )?;
        Ok(json!({"id": id, "code": p.code, "currency": p.currency}))
    })
}

/// Creates a purchase requisition in `pending` status. [procurement.requisitions.create]
pub fn create_requisition(
    conn: &Connection,
    ctx: &Ctx,
    item_id: &str,
    qty_kth: i64,
    est_unit_cost_minor: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.requisitions.create")?;
        if qty_kth <= 0 {
            return Err("field 'qty_kth' must be greater than zero".to_string());
        }
        if est_unit_cost_minor < 0 {
            return Err(
                "field 'est_unit_cost_minor' must be greater than or equal to zero".to_string(),
            );
        }
        let exists: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM items WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, item_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !exists {
            return Err(format!(
                "field 'item_id' references unknown item '{}'",
                item_id
            ));
        }
        let amount = mmul_qty(est_unit_cost_minor, qty_kth)?;
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "PR", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO requisitions (tenant_id, id, number, item_id, qty_kth, est_unit_cost_minor, amount_minor, requester, status, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9)",
            params![ctx.tenant, id, number, item_id, qty_kth, est_unit_cost_minor, amount, ctx.user, now],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "requisition.created",
            &id,
            &json!({"number": number, "amount_minor": amount}),
            now,
        )?;
        Ok(json!({"id": id, "number": number, "amount_minor": amount, "status": "pending"}))
    })
}

/// Approves a pending or escalated requisition; on escalation threshold breach it escalates instead of failing. [procurement.requisitions.approve]
pub fn approve_requisition(
    conn: &Connection,
    ctx: &Ctx,
    requisition_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.requisitions.approve")?;
        let (number, amount, status): (String, i64, String) = tx
            .query_row(
                "SELECT number, amount_minor, status FROM requisitions WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, requisition_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("requisition '{}' not found", requisition_id))?;
        if status != "pending" && status != "escalated" {
            return Err(format!(
                "field 'status' must be 'pending' or 'escalated' to approve, got '{}'",
                status
            ));
        }
        match check_approval(tx, ctx, amount) {
            Ok(()) => {
                tx.execute(
                    "UPDATE requisitions SET status = 'approved', approver = ?3 WHERE tenant_id = ?1 AND id = ?2",
                    params![ctx.tenant, requisition_id, ctx.user],
                )
                .map_err(e)?;
                audit(
                    tx,
                    &ctx.tenant,
                    "requisition.approved",
                    requisition_id,
                    &json!({"number": number, "amount_minor": amount}),
                    now,
                )?;
                Ok(
                    json!({"id": requisition_id, "number": number, "status": "approved", "approver": ctx.user}),
                )
            }
            Err(msg) => {
                if msg.contains("escalation required") {
                    tx.execute(
                        "UPDATE requisitions SET status = 'escalated' WHERE tenant_id = ?1 AND id = ?2",
                        params![ctx.tenant, requisition_id],
                    )
                    .map_err(e)?;
                    notify(
                        tx,
                        &ctx.tenant,
                        "procurement-manager",
                        "requisition.escalated",
                        &json!({"requisition_id": requisition_id, "number": number, "amount_minor": amount}),
                        now,
                    )?;
                    audit(
                        tx,
                        &ctx.tenant,
                        "requisition.escalated",
                        requisition_id,
                        &json!({"number": number, "amount_minor": amount}),
                        now,
                    )?;
                    return Ok(
                        json!({"id": requisition_id, "number": number, "status": "escalated", "amount_minor": amount}),
                    );
                }
                Err(msg)
            }
        }
    })
}

/// Rejects a pending or escalated requisition with a reason. [procurement.requisitions.reject]
pub fn reject_requisition(
    conn: &Connection,
    ctx: &Ctx,
    requisition_id: &str,
    reason: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.requisitions.reject")?;
        let (number, status): (String, String) = tx
            .query_row(
                "SELECT number, status FROM requisitions WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, requisition_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("requisition '{}' not found", requisition_id))?;
        if status != "pending" && status != "escalated" {
            return Err(format!(
                "field 'status' must be 'pending' or 'escalated' to reject, got '{}'",
                status
            ));
        }
        tx.execute(
            "UPDATE requisitions SET status = 'rejected' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, requisition_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "requisition.rejected",
            requisition_id,
            &json!({"number": number, "reason": reason}),
            now,
        )?;
        Ok(json!({"id": requisition_id, "number": number, "status": "rejected", "reason": reason}))
    })
}

/// Creates an RFQ from an approved requisition. [procurement.rfqs.create]
pub fn create_rfq(conn: &Connection, ctx: &Ctx, requisition_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.rfqs.create")?;
        let status: String = tx
            .query_row(
                "SELECT status FROM requisitions WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, requisition_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("requisition '{}' not found", requisition_id))?;
        if status != "approved" {
            return Err(format!(
                "field 'status' must be 'approved' to create an RFQ, got '{}'",
                status
            ));
        }
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "RFQ", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO rfqs (tenant_id, id, number, requisition_id, status, created_at) VALUES (?1,?2,?3,?4,'open',?5)",
            params![ctx.tenant, id, number, requisition_id, now],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "rfq.created",
            &id,
            &json!({"number": number, "requisition_id": requisition_id}),
            now,
        )?;
        Ok(json!({"id": id, "number": number, "status": "open"}))
    })
}

/// Adds a supplier quote to an open RFQ; one quote per supplier per RFQ. [procurement.rfqs.quote]
pub fn add_rfq_quote(
    conn: &Connection,
    ctx: &Ctx,
    rfq_id: &str,
    supplier_id: &str,
    unit_price_minor: i64,
    lead_time_days: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.rfqs.quote")?;
        if unit_price_minor < 0 {
            return Err(
                "field 'unit_price_minor' must be greater than or equal to zero".to_string(),
            );
        }
        if lead_time_days < 0 {
            return Err("field 'lead_time_days' must be greater than or equal to zero".to_string());
        }
        let status: String = tx
            .query_row(
                "SELECT status FROM rfqs WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, rfq_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("rfq '{}' not found", rfq_id))?;
        if status != "open" {
            return Err(format!(
                "field 'status' must be 'open' to add a quote, got '{}'",
                status
            ));
        }
        let supplier_ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM suppliers WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, supplier_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !supplier_ok {
            return Err(format!(
                "field 'supplier_id' references unknown supplier '{}'",
                supplier_id
            ));
        }
        let dup: bool = tx
            .query_row("SELECT COUNT(*) FROM rfq_quotes WHERE tenant_id = ?1 AND rfq_id = ?2 AND supplier_id = ?3", params![ctx.tenant, rfq_id, supplier_id], |r| r.get::<_, i64>(0))
            .map_err(e)? > 0;
        if dup {
            return Err(format!(
                "a quote from supplier '{}' already exists for RFQ '{}'",
                supplier_id, rfq_id
            ));
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO rfq_quotes (tenant_id, id, rfq_id, supplier_id, unit_price_minor, lead_time_days, status, created_at) VALUES (?1,?2,?3,?4,?5,?6,'received',?7)",
            params![ctx.tenant, id, rfq_id, supplier_id, unit_price_minor, lead_time_days, now],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "rfq.quote_added",
            &id,
            &json!({"rfq_id": rfq_id, "supplier_id": supplier_id, "unit_price_minor": unit_price_minor}),
            now,
        )?;
        Ok(json!({"id": id, "rfq_id": rfq_id, "supplier_id": supplier_id, "status": "received"}))
    })
}

/// Read helper: ranks quotes of an RFQ by price then lead time.
pub fn compare_quotes(conn: &Connection, tenant: &str, rfq_id: &str) -> Res<Vec<Value>> {
    let mut stmt = conn
        .prepare("SELECT id, supplier_id, unit_price_minor, lead_time_days FROM rfq_quotes WHERE tenant_id = ?1 AND rfq_id = ?2 ORDER BY unit_price_minor ASC, lead_time_days ASC, created_at ASC")
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant, rfq_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut out = Vec::with_capacity(rows.len());
    for (i, (quote_id, supplier_id, price, lead)) in rows.into_iter().enumerate() {
        out.push(json!({"quote_id": quote_id, "supplier_id": supplier_id, "unit_price_minor": price, "lead_time_days": lead, "rank": i + 1}));
    }
    Ok(out)
}

/// Awards an RFQ to the chosen quote and rejects the rest. [procurement.rfqs.award]
pub fn award_rfq(
    conn: &Connection,
    ctx: &Ctx,
    rfq_id: &str,
    quote_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.rfqs.award")?;
        let status: String = tx
            .query_row(
                "SELECT status FROM rfqs WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, rfq_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("rfq '{}' not found", rfq_id))?;
        if status != "open" {
            return Err(format!(
                "field 'status' must be 'open' to award, got '{}'",
                status
            ));
        }
        let quote_rfq: String = tx
            .query_row(
                "SELECT rfq_id FROM rfq_quotes WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, quote_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("quote '{}' not found", quote_id))?;
        if quote_rfq != rfq_id {
            return Err(format!(
                "field 'quote_id' quote '{}' does not belong to RFQ '{}'",
                quote_id, rfq_id
            ));
        }
        tx.execute("UPDATE rfq_quotes SET status = 'rejected' WHERE tenant_id = ?1 AND rfq_id = ?2 AND id != ?3", params![ctx.tenant, rfq_id, quote_id]).map_err(e)?;
        tx.execute(
            "UPDATE rfq_quotes SET status = 'awarded' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, quote_id],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE rfqs SET status = 'awarded' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, rfq_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "rfq.awarded",
            rfq_id,
            &json!({"quote_id": quote_id}),
            now,
        )?;
        Ok(json!({"id": rfq_id, "status": "awarded", "quote_id": quote_id}))
    })
}

/// One line of a purchase order being created.
pub struct PoLineIn {
    pub item_id: String,
    pub qty_kth: i64,
    pub unit_cost_minor: i64,
}

/// Creates an open purchase order with at least one line. [procurement.po.create]
pub fn create_po(
    conn: &Connection,
    ctx: &Ctx,
    supplier_id: &str,
    lines: &[PoLineIn],
    expected_date: Option<&str>,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let lines_json: Vec<Value> = lines
        .iter()
        .map(|l| json!({"item_id": l.item_id, "qty_kth": l.qty_kth, "unit_cost_minor": l.unit_cost_minor}))
        .collect();
    let payload =
        json!({"supplier_id": supplier_id, "lines": lines_json, "expected_date": expected_date});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.po.create")?;
        if lines.is_empty() {
            return Err("field 'lines' must contain at least one line".to_string());
        }
        for (i, l) in lines.iter().enumerate() {
            if l.qty_kth <= 0 {
                return Err(format!(
                    "field 'lines[{}].qty_kth' must be greater than zero",
                    i
                ));
            }
            if l.unit_cost_minor < 0 {
                return Err(format!(
                    "field 'lines[{}].unit_cost_minor' must be greater than or equal to zero",
                    i
                ));
            }
        }
        let currency: String = tx
            .query_row(
                "SELECT currency FROM suppliers WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, supplier_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("supplier '{}' not found", supplier_id))?;
        let mut total: i64 = 0;
        for l in lines {
            let line_total = mmul_qty(l.unit_cost_minor, l.qty_kth)?;
            total = madd(total, line_total)?;
        }
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "PO", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO purchase_orders (tenant_id, id, number, supplier_id, rfq_id, status, currency, total_minor, expected_date, created_by, created_at) VALUES (?1,?2,?3,?4,NULL,'open',?5,?6,?7,?8,?9)",
            params![ctx.tenant, id, number, supplier_id, currency, total, expected_date, ctx.user, now],
        )
        .map_err(e)?;
        for (i, l) in lines.iter().enumerate() {
            tx.execute(
                "INSERT INTO purchase_order_lines (tenant_id, po_id, line_no, item_id, qty_kth, qty_received_kth, unit_cost_minor) VALUES (?1,?2,?3,?4,?5,0,?6)",
                params![ctx.tenant, id, (i as i64) + 1, l.item_id, l.qty_kth, l.unit_cost_minor],
            )
            .map_err(e)?;
        }
        let result = json!({"id": id, "number": number, "status": "open", "total_minor": total, "currency": currency});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "po.created",
            &id,
            &json!({"number": number, "total_minor": total}),
            now,
        )?;
        Ok(result)
    })
}

/// Creates a purchase order from an awarded RFQ (one line at the awarded price). [procurement.po.create]
pub fn po_from_rfq(
    conn: &Connection,
    ctx: &Ctx,
    rfq_id: &str,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let payload = json!({"rfq_id": rfq_id});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.po.create")?;
        let (requisition_id, status): (String, String) = tx
            .query_row(
                "SELECT requisition_id, status FROM rfqs WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, rfq_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("rfq '{}' not found", rfq_id))?;
        if status != "awarded" {
            return Err(format!(
                "field 'status' must be 'awarded' to create a PO, got '{}'",
                status
            ));
        }
        let (quote_id, supplier_id, price): (String, String, i64) = tx
            .query_row("SELECT id, supplier_id, unit_price_minor FROM rfq_quotes WHERE tenant_id = ?1 AND rfq_id = ?2 AND status = 'awarded'", params![ctx.tenant, rfq_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("no awarded quote found for RFQ '{}'", rfq_id))?;
        let (item_id, qty_kth): (String, i64) = tx
            .query_row(
                "SELECT item_id, qty_kth FROM requisitions WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, requisition_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("requisition '{}' not found", requisition_id))?;
        let currency: String = tx
            .query_row(
                "SELECT currency FROM suppliers WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, supplier_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("supplier '{}' not found", supplier_id))?;
        let total = mmul_qty(price, qty_kth)?;
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "PO", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO purchase_orders (tenant_id, id, number, supplier_id, rfq_id, status, currency, total_minor, expected_date, created_by, created_at) VALUES (?1,?2,?3,?4,?5,'open',?6,?7,NULL,?8,?9)",
            params![ctx.tenant, id, number, supplier_id, rfq_id, currency, total, ctx.user, now],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO purchase_order_lines (tenant_id, po_id, line_no, item_id, qty_kth, qty_received_kth, unit_cost_minor) VALUES (?1,?2,1,?3,?4,0,?5)",
            params![ctx.tenant, id, item_id, qty_kth, price],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE requisitions SET status = 'converted' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, requisition_id],
        )
        .map_err(e)?;
        let result = json!({"id": id, "number": number, "status": "open", "total_minor": total, "rfq_id": rfq_id, "quote_id": quote_id, "requisition_id": requisition_id});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "po.from_rfq",
            &id,
            &json!({"number": number, "rfq_id": rfq_id}),
            now,
        )?;
        Ok(result)
    })
}

/// One line of a goods receipt.
pub struct GrLineIn {
    pub po_line_no: i64,
    pub qty_kth: i64,
    pub batch_no: Option<String>,
    pub batch_expiry: Option<String>,
}

/// Receives goods against an open or partially received PO (110% line tolerance). [procurement.po.receive]
pub fn receive_goods(
    conn: &Connection,
    ctx: &Ctx,
    po_id: &str,
    warehouse_id: &str,
    lines: &[GrLineIn],
    ledger: &dyn Ledger,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let lines_json: Vec<Value> = lines
        .iter()
        .map(|l| json!({"po_line_no": l.po_line_no, "qty_kth": l.qty_kth, "batch_no": l.batch_no}))
        .collect();
    let payload = json!({"po_id": po_id, "warehouse_id": warehouse_id, "lines": lines_json});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.po.receive")?;
        if lines.is_empty() {
            return Err("field 'lines' must contain at least one line".to_string());
        }
        for (i, l) in lines.iter().enumerate() {
            if l.qty_kth <= 0 {
                return Err(format!(
                    "field 'lines[{}].qty_kth' must be greater than zero",
                    i
                ));
            }
        }
        let (po_number, currency, po_status): (String, String, String) = tx
            .query_row("SELECT number, currency, status FROM purchase_orders WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, po_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("purchase order '{}' not found", po_id))?;
        if po_status != "open" && po_status != "partially_received" {
            return Err(format!(
                "field 'status' must be 'open' or 'partially_received' to receive goods, got '{}'",
                po_status
            ));
        }
        let company = ctx.company.as_deref().unwrap_or("default");
        let gr_number = next_number(tx, &ctx.tenant, company, "GR", now)?;
        let gr_id = new_id();
        tx.execute(
            "INSERT INTO goods_receipts (tenant_id, id, number, po_id, warehouse_id, status, received_at, created_by) VALUES (?1,?2,?3,?4,?5,'posted',?6,?7)",
            params![ctx.tenant, gr_id, gr_number, po_id, warehouse_id, now, ctx.user],
        )
        .map_err(e)?;
        let mut received_value: i64 = 0;
        for (i, line) in lines.iter().enumerate() {
            let (item_id, ordered, already, unit_cost): (String, i64, i64, i64) = tx
                .query_row(
                    "SELECT item_id, qty_kth, qty_received_kth, unit_cost_minor FROM purchase_order_lines WHERE tenant_id = ?1 AND po_id = ?2 AND line_no = ?3",
                    params![ctx.tenant, po_id, line.po_line_no],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(e)?
                .ok_or_else(|| format!("field 'lines[{}].po_line_no' references unknown PO line {}", i, line.po_line_no))?;
            let new_received = qadd(already, line.qty_kth)?;
            let tolerated = (ordered as i128) * 11 / 10;
            if (new_received as i128) > tolerated {
                return Err(format!("field 'lines[{}].qty_kth' exceeds the 110% tolerance on line {}: ordered {} thousandths, receipt would bring received to {}", i, line.po_line_no, ordered, new_received));
            }
            inventory::receive_material(
                tx,
                ctx,
                &inventory::ReceiveParams {
                    item_id: item_id.clone(),
                    warehouse_id: warehouse_id.to_string(),
                    qty_kth: line.qty_kth,
                    unit_cost_minor: unit_cost,
                    batch_no: line.batch_no.clone(),
                    batch_expiry: line.batch_expiry.clone(),
                    ref_type: Some("purchase_order".to_string()),
                    ref_id: Some(po_id.to_string()),
                },
                now,
            )?;
            tx.execute(
                "UPDATE purchase_order_lines SET qty_received_kth = ?4 WHERE tenant_id = ?1 AND po_id = ?2 AND line_no = ?3",
                params![ctx.tenant, po_id, line.po_line_no, new_received],
            )
            .map_err(e)?;
            tx.execute(
                "INSERT INTO goods_receipt_lines (tenant_id, gr_id, line_no, po_line_no, item_id, qty_kth, batch_no) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![ctx.tenant, gr_id, (i as i64) + 1, line.po_line_no, item_id, line.qty_kth, line.batch_no],
            )
            .map_err(e)?;
            let line_value = mmul_qty(unit_cost, line.qty_kth)?;
            received_value = madd(received_value, line_value)?;
        }
        let (line_count, fully_received): (i64, i64) = tx
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN qty_received_kth >= qty_kth THEN 1 ELSE 0 END), 0) FROM purchase_order_lines WHERE tenant_id = ?1 AND po_id = ?2",
                params![ctx.tenant, po_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(e)?;
        let new_status = if line_count > 0 && fully_received == line_count {
            "received"
        } else {
            "partially_received"
        };
        tx.execute(
            "UPDATE purchase_orders SET status = ?3 WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, po_id, new_status],
        )
        .map_err(e)?;
        if received_value > 0 {
            ledger.post(
                &ctx.tenant,
                &JournalEntry {
                    date: today(now),
                    memo: format!("Goods receipt {} against PO {}", gr_number, po_number),
                    currency: currency.clone(),
                    lines: vec![
                        JournalLine {
                            account: "1300".to_string(),
                            debit: received_value,
                            credit: 0,
                        },
                        JournalLine {
                            account: "2050".to_string(),
                            debit: 0,
                            credit: received_value,
                        },
                    ],
                },
            )?;
        }
        let result = json!({"id": gr_id, "number": gr_number, "status": "posted", "po_id": po_id, "po_status": new_status, "received_value_minor": received_value});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "goods.received",
            &gr_id,
            &json!({"number": gr_number, "po_id": po_id, "received_value_minor": received_value}),
            now,
        )?;
        Ok(result)
    })
}

/// Creates a supplier bill with three-way match against the PO; flags price variance. [procurement.bills.create]
pub fn create_supplier_bill(
    conn: &Connection,
    ctx: &Ctx,
    number: &str,
    supplier_id: &str,
    po_id: &str,
    total_minor: i64,
    bill_date: &str,
    ledger: &dyn Ledger,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let payload = json!({"number": number, "supplier_id": supplier_id, "po_id": po_id, "total_minor": total_minor, "bill_date": bill_date});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.bills.create")?;
        if number.trim().is_empty() {
            return Err("field 'number' must not be empty".to_string());
        }
        if total_minor < 0 {
            return Err("field 'total_minor' must be greater than or equal to zero".to_string());
        }
        if bill_date.trim().is_empty() {
            return Err("field 'bill_date' must not be empty".to_string());
        }
        let dup: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM supplier_bills WHERE tenant_id = ?1 AND number = ?2",
                params![ctx.tenant, number],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if dup {
            return Err(format!(
                "field 'number' bill '{}' already exists for this tenant",
                number
            ));
        }
        let (currency, po_exists): (String, i64) = tx
            .query_row(
                "SELECT currency, 1 FROM purchase_orders WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, po_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("purchase order '{}' not found", po_id))?;
        let _ = po_exists;
        let supplier_ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM suppliers WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, supplier_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !supplier_ok {
            return Err(format!(
                "field 'supplier_id' references unknown supplier '{}'",
                supplier_id
            ));
        }
        let mut received_value: i64 = 0;
        let mut any_received = false;
        {
            let mut stmt = tx
                .prepare("SELECT unit_cost_minor, qty_received_kth FROM purchase_order_lines WHERE tenant_id = ?1 AND po_id = ?2")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant, po_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            for (cost, received) in rows {
                if received > 0 {
                    any_received = true;
                    received_value = madd(received_value, mmul_qty(cost, received)?)?;
                }
            }
        }
        if !any_received {
            return Err(format!(
                "purchase order '{}' has no goods received; cannot create a bill",
                po_id
            ));
        }
        let variance = msub(total_minor, received_value)?;
        let match_status = if variance == 0 { "matched" } else { "flagged" };
        let id = new_id();
        tx.execute(
            "INSERT INTO supplier_bills (tenant_id, id, number, supplier_id, po_id, total_minor, price_variance_minor, match_status, status, bill_date, created_by, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'posted',?9,?10,?11)",
            params![ctx.tenant, id, number, supplier_id, po_id, total_minor, variance, match_status, bill_date, ctx.user, now],
        )
        .map_err(e)?;
        let mut lines = vec![JournalLine {
            account: "2050".to_string(),
            debit: received_value,
            credit: 0,
        }];
        if variance > 0 {
            lines.push(JournalLine {
                account: "5100".to_string(),
                debit: variance,
                credit: 0,
            });
        } else if variance < 0 {
            lines.push(JournalLine {
                account: "5100".to_string(),
                debit: 0,
                credit: variance.saturating_neg(),
            });
        }
        lines.push(JournalLine {
            account: "2000".to_string(),
            debit: 0,
            credit: total_minor,
        });
        ledger.post(
            &ctx.tenant,
            &JournalEntry {
                date: bill_date.to_string(),
                memo: format!("Supplier bill {}", number),
                currency,
                lines,
            },
        )?;
        let result = json!({"id": id, "number": number, "status": "posted", "match_status": match_status, "price_variance_minor": variance, "received_value_minor": received_value});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "bill.created",
            &id,
            &json!({"number": number, "match_status": match_status, "price_variance_minor": variance}),
            now,
        )?;
        Ok(result)
    })
}

/// Approves a flagged bill variance after an approval check. [procurement.bills.approve]
pub fn approve_bill_variance(conn: &Connection, ctx: &Ctx, bill_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.bills.approve")?;
        let (match_status, variance): (String, i64) = tx
            .query_row("SELECT match_status, price_variance_minor FROM supplier_bills WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, bill_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("bill '{}' not found", bill_id))?;
        if match_status != "flagged" {
            return Err(format!(
                "field 'match_status' must be 'flagged' to approve variance, got '{}'",
                match_status
            ));
        }
        check_approval(tx, ctx, variance.saturating_abs())?;
        tx.execute(
            "UPDATE supplier_bills SET match_status = 'approved' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, bill_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "bill.variance_approved",
            bill_id,
            &json!({"price_variance_minor": variance}),
            now,
        )?;
        Ok(json!({"id": bill_id, "match_status": "approved", "price_variance_minor": variance}))
    })
}

/// Creates a payment proposal from eligible posted bills (matched or approved variance). [procurement.payments.propose]
pub fn create_payment_proposal(conn: &Connection, ctx: &Ctx, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.payments.propose")?;
        let mut stmt = tx
            .prepare(
                "SELECT b.id, b.total_minor, b.number FROM supplier_bills b WHERE b.tenant_id = ?1 AND b.status = 'posted' AND b.match_status IN ('matched','approved') AND NOT EXISTS (SELECT 1 FROM payment_proposal_lines l WHERE l.tenant_id = b.tenant_id AND l.bill_id = b.id) ORDER BY b.created_at ASC",
            )
            .map_err(e)?;
        let bills = stmt
            .query_map(params![ctx.tenant], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        if bills.is_empty() {
            return Err("no eligible bills for a payment proposal".to_string());
        }
        let mut total: i64 = 0;
        let mut lines = Vec::with_capacity(bills.len());
        for (bill_id, amount, bill_number) in &bills {
            total = madd(total, *amount)?;
            lines.push(
                json!({"bill_id": bill_id, "bill_number": bill_number, "amount_minor": amount}),
            );
        }
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "PAY", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO payment_proposals (tenant_id, id, number, status, created_by, created_at) VALUES (?1,?2,?3,'draft',?4,?5)",
            params![ctx.tenant, id, number, ctx.user, now],
        )
        .map_err(e)?;
        for (bill_id, amount, _) in &bills {
            tx.execute(
                "INSERT INTO payment_proposal_lines (tenant_id, proposal_id, bill_id, amount_minor) VALUES (?1,?2,?3,?4)",
                params![ctx.tenant, id, bill_id, amount],
            )
            .map_err(e)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "payment_proposal.created",
            &id,
            &json!({"number": number, "total_minor": total, "bills": bills.len()}),
            now,
        )?;
        Ok(
            json!({"id": id, "number": number, "status": "draft", "total_minor": total, "lines": lines}),
        )
    })
}

/// Approves a payment proposal: posts Dr 2000 / Cr 1000 and marks bills paid. [procurement.payments.approve]
pub fn approve_payment_proposal(
    conn: &Connection,
    ctx: &Ctx,
    proposal_id: &str,
    ledger: &dyn Ledger,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "procurement.payments.approve")?;
        let (number, status): (String, String) = tx
            .query_row(
                "SELECT number, status FROM payment_proposals WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, proposal_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("payment proposal '{}' not found", proposal_id))?;
        if status != "draft" {
            return Err(format!(
                "field 'status' must be 'draft' to approve, got '{}'",
                status
            ));
        }
        let currency: String = tx
            .query_row("SELECT COALESCE((SELECT s.currency FROM suppliers s JOIN supplier_bills b ON b.tenant_id = s.tenant_id AND b.supplier_id = s.id WHERE b.tenant_id = ?1 AND b.id = (SELECT bill_id FROM payment_proposal_lines WHERE tenant_id = ?1 AND proposal_id = ?2 LIMIT 1)), 'USD')", params![ctx.tenant, proposal_id], |r| r.get(0))
            .map_err(e)?;
        let mut total: i64 = 0;
        let mut bill_ids: Vec<String> = Vec::new();
        {
            let mut stmt = tx
                .prepare("SELECT bill_id, amount_minor FROM payment_proposal_lines WHERE tenant_id = ?1 AND proposal_id = ?2")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant, proposal_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            for (bill_id, amount) in rows {
                total = madd(total, amount)?;
                bill_ids.push(bill_id);
            }
        }
        if total <= 0 {
            return Err(format!(
                "payment proposal '{}' has no payable amount",
                proposal_id
            ));
        }
        check_approval(tx, ctx, total)?;
        ledger.post(
            &ctx.tenant,
            &JournalEntry {
                date: today(now),
                memo: format!("Payment proposal {}", number),
                currency,
                lines: vec![
                    JournalLine {
                        account: "2000".to_string(),
                        debit: total,
                        credit: 0,
                    },
                    JournalLine {
                        account: "1000".to_string(),
                        debit: 0,
                        credit: total,
                    },
                ],
            },
        )?;
        tx.execute("UPDATE payment_proposals SET status = 'approved', approved_by = ?3 WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, proposal_id, ctx.user]).map_err(e)?;
        for bill_id in &bill_ids {
            tx.execute(
                "UPDATE supplier_bills SET status = 'paid' WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, bill_id],
            )
            .map_err(e)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "payment_proposal.approved",
            proposal_id,
            &json!({"number": number, "total_minor": total, "bills": bill_ids.len()}),
            now,
        )?;
        Ok(
            json!({"id": proposal_id, "number": number, "status": "approved", "approved_by": ctx.user, "total_minor": total}),
        )
    })
}

/// Read helper: open PO lines (received < ordered) on non-cancelled, non-closed POs.
pub fn open_po_lines(conn: &Connection, tenant: &str) -> Res<Vec<Value>> {
    let mut stmt = conn
        .prepare(
            "SELECT po.number, l.item_id, l.qty_kth - l.qty_received_kth, po.expected_date FROM purchase_order_lines l JOIN purchase_orders po ON po.tenant_id = l.tenant_id AND po.id = l.po_id WHERE po.tenant_id = ?1 AND po.status IN ('open','partially_received') AND l.qty_received_kth < l.qty_kth ORDER BY po.created_at ASC, l.line_no ASC",
        )
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut out = Vec::with_capacity(rows.len());
    for (po_number, item_id, qty_open, expected) in rows {
        out.push(json!({"po_number": po_number, "item_id": item_id, "qty_open_kth": qty_open, "expected_date": expected}));
    }
    Ok(out)
}

/// Read helper: open commitment value bucketed by PO age in days.
pub fn purchase_ageing(conn: &Connection, tenant: &str, as_of: i64) -> Res<Value> {
    let mut stmt = conn
        .prepare(
            "SELECT po.created_at, l.qty_kth - l.qty_received_kth, l.unit_cost_minor FROM purchase_order_lines l JOIN purchase_orders po ON po.tenant_id = l.tenant_id AND po.id = l.po_id WHERE po.tenant_id = ?1 AND po.status IN ('open','partially_received') AND l.qty_received_kth < l.qty_kth",
        )
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut b0: i64 = 0;
    let mut b31: i64 = 0;
    let mut b61: i64 = 0;
    let mut b90: i64 = 0;
    for (created_at, qty_open, cost) in rows {
        let age_days = as_of.saturating_sub(created_at).div_euclid(86400);
        let value = mmul_qty(cost, qty_open)?;
        if age_days <= 30 {
            b0 = madd(b0, value)?;
        } else if age_days <= 60 {
            b31 = madd(b31, value)?;
        } else if age_days <= 90 {
            b61 = madd(b61, value)?;
        } else {
            b90 = madd(b90, value)?;
        }
    }
    let total = madd(madd(madd(b0, b31)?, b61)?, b90)?;
    Ok(json!({"0-30": b0, "31-60": b31, "61-90": b61, "90+": b90, "total": total}))
}
