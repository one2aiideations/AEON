//! Bills of materials, work orders, MRP and quality holds.

use crate::common::*;
use crate::inventory;
use crate::procurement;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

fn today(now: i64) -> String {
    ymd(now).chars().take(10).collect()
}

/// Updates the lead time (days) of an item. [manufacturing.items.lead_time]
pub fn set_item_lead_time(
    conn: &Connection,
    ctx: &Ctx,
    item_id: &str,
    days: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.items.lead_time")?;
        if days < 0 {
            return Err("field 'days' must be greater than or equal to zero".to_string());
        }
        let n = tx
            .execute(
                "UPDATE items SET lead_time_days = ?3 WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, item_id, days],
            )
            .map_err(e)?;
        if n == 0 {
            return Err(format!("item '{}' not found", item_id));
        }
        audit(
            tx,
            &ctx.tenant,
            "item.lead_time_set",
            item_id,
            &json!({"lead_time_days": days}),
            now,
        )?;
        Ok(json!({"id": item_id, "lead_time_days": days}))
    })
}

/// One component line of a new BOM.
pub struct BomLineIn {
    pub component_id: String,
    pub qty_kth: i64,
    pub scrap_kth: i64,
}

/// Creates a draft BOM with version = previous max + 1. [manufacturing.bom.create]
pub fn create_bom(
    conn: &Connection,
    ctx: &Ctx,
    item_id: &str,
    header_scrap_kth: i64,
    lines: &[BomLineIn],
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.bom.create")?;
        if lines.is_empty() {
            return Err("field 'lines' must contain at least one line".to_string());
        }
        if !(0..=100_000).contains(&header_scrap_kth) {
            return Err(
                "field 'header_scrap_kth' must be between 0 and 100000 thousandths".to_string(),
            );
        }
        for (i, l) in lines.iter().enumerate() {
            if l.qty_kth <= 0 {
                return Err(format!(
                    "field 'lines[{}].qty_kth' must be greater than zero",
                    i
                ));
            }
            if l.scrap_kth < 0 || l.scrap_kth > 100_000 {
                return Err(format!(
                    "field 'lines[{}].scrap_kth' must be between 0 and 100000 thousandths",
                    i
                ));
            }
            if l.component_id == item_id {
                return Err(format!(
                    "field 'lines[{}].component_id' must not equal the bom item itself ('{}')",
                    i, item_id
                ));
            }
        }
        let item_ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM items WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, item_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !item_ok {
            return Err(format!("item '{}' not found", item_id));
        }
        let max_version: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM boms WHERE tenant_id = ?1 AND item_id = ?2",
                params![ctx.tenant, item_id],
                |r| r.get(0),
            )
            .map_err(e)?;
        let version = max_version.checked_add(1).ok_or("bom version overflow")?;
        let id = new_id();
        tx.execute(
            "INSERT INTO boms (tenant_id, id, item_id, version, scrap_kth, status, created_at) VALUES (?1,?2,?3,?4,?5,'draft',?6)",
            params![ctx.tenant, id, item_id, version, header_scrap_kth, now],
        )
        .map_err(e)?;
        for (i, l) in lines.iter().enumerate() {
            tx.execute(
                "INSERT INTO bom_lines (tenant_id, bom_id, line_no, component_id, qty_kth, scrap_kth) VALUES (?1,?2,?3,?4,?5,?6)",
                params![ctx.tenant, id, (i as i64) + 1, l.component_id, l.qty_kth, l.scrap_kth],
            )
            .map_err(e)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "bom.created",
            &id,
            &json!({"item_id": item_id, "version": version}),
            now,
        )?;
        Ok(json!({"id": id, "item_id": item_id, "version": version, "status": "draft"}))
    })
}

fn released_bom_of(conn: &Connection, tenant: &str, item_id: &str) -> Res<Option<(String, i64)>> {
    conn.query_row(
        "SELECT id, scrap_kth FROM boms WHERE tenant_id = ?1 AND item_id = ?2 AND status = 'released' ORDER BY version DESC LIMIT 1",
        params![tenant, item_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(e)
}

fn component_reaches(
    conn: &Connection,
    tenant: &str,
    from_item: &str,
    target: &str,
    depth: i64,
) -> Res<bool> {
    if depth > 20 {
        return Err(format!(
            "bom cycle check exceeded depth limit of 20 while expanding item '{}'",
            from_item
        ));
    }
    if let Some((bom_id, _)) = released_bom_of(conn, tenant, from_item)? {
        let mut stmt = conn
            .prepare("SELECT component_id FROM bom_lines WHERE tenant_id = ?1 AND bom_id = ?2")
            .map_err(e)?;
        let comps = stmt
            .query_map(params![tenant, bom_id], |r| r.get::<_, String>(0))
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        for c in comps {
            if c == target {
                return Ok(true);
            }
            if component_reaches(conn, tenant, &c, target, depth + 1)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Releases a draft BOM; rejects cycles through released component BOMs. [manufacturing.bom.release]
pub fn release_bom(conn: &Connection, ctx: &Ctx, bom_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.bom.release")?;
        let (item_id, status): (String, String) = tx
            .query_row(
                "SELECT item_id, status FROM boms WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, bom_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("bom '{}' not found", bom_id))?;
        if status != "draft" {
            return Err(format!(
                "field 'status' must be 'draft' to release, got '{}'",
                status
            ));
        }
        let mut stmt = tx
            .prepare("SELECT component_id FROM bom_lines WHERE tenant_id = ?1 AND bom_id = ?2")
            .map_err(e)?;
        let comps = stmt
            .query_map(params![ctx.tenant, bom_id], |r| r.get::<_, String>(0))
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        for c in &comps {
            if c == &item_id {
                return Err(format!(
                    "bom release would create a cycle: item '{}' is its own component",
                    item_id
                ));
            }
            if component_reaches(tx, &ctx.tenant, c, &item_id, 1)? {
                return Err(format!("bom release would create a cycle: item '{}' depends (through released boms) on '{}', which depends on '{}'", item_id, c, item_id));
            }
        }
        tx.execute("UPDATE boms SET status = 'obsolete' WHERE tenant_id = ?1 AND item_id = ?2 AND status = 'released'", params![ctx.tenant, item_id]).map_err(e)?;
        tx.execute(
            "UPDATE boms SET status = 'released' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, bom_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "bom.released",
            bom_id,
            &json!({"item_id": item_id}),
            now,
        )?;
        Ok(json!({"id": bom_id, "item_id": item_id, "status": "released"}))
    })
}

fn req_qty(
    parent_qty_kth: i64,
    line_qty_kth: i64,
    line_scrap_kth: i64,
    header_scrap_kth: i64,
) -> Res<i64> {
    let num = (parent_qty_kth as i128)
        .checked_mul(line_qty_kth as i128)
        .and_then(|v| v.checked_mul((100_000 + line_scrap_kth) as i128))
        .and_then(|v| v.checked_mul((100_000 + header_scrap_kth) as i128))
        .ok_or("bom requirement arithmetic overflow")?;
    let den: i128 = 1000i128 * 100_000i128 * 100_000i128;
    let ceil = (num + den - 1) / den;
    i64::try_from(ceil).map_err(e)
}

fn explode_inner(
    conn: &Connection,
    tenant: &str,
    item_id: &str,
    qty_kth: i64,
    depth: i64,
    leaves: &mut HashMap<String, i64>,
) -> Res<()> {
    if depth > 20 {
        return Err(format!(
            "bom explosion exceeded depth limit of 20 at item '{}'",
            item_id
        ));
    }
    match released_bom_of(conn, tenant, item_id)? {
        Some((bom_id, header_scrap)) => {
            let mut stmt = conn
                .prepare("SELECT component_id, qty_kth, scrap_kth FROM bom_lines WHERE tenant_id = ?1 AND bom_id = ?2 ORDER BY line_no ASC")
                .map_err(e)?;
            let lines = stmt
                .query_map(params![tenant, bom_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            for (component, line_qty, line_scrap) in lines {
                let req = req_qty(qty_kth, line_qty, line_scrap, header_scrap)?;
                explode_inner(conn, tenant, &component, req, depth + 1, leaves)?;
            }
            Ok(())
        }
        None => {
            let entry = leaves.entry(item_id.to_string()).or_insert(0);
            *entry = entry
                .checked_add(qty_kth)
                .ok_or("quantity overflow in bom explosion")?;
            Ok(())
        }
    }
}

/// Read helper: fully explodes a released BOM to leaf items; quantities summed per leaf.
pub fn explode_bom(
    conn: &Connection,
    tenant: &str,
    item_id: &str,
    qty_kth: i64,
) -> Res<Vec<Value>> {
    if qty_kth <= 0 {
        return Err("field 'qty_kth' must be greater than zero".to_string());
    }
    let mut leaves: HashMap<String, i64> = HashMap::new();
    explode_inner(conn, tenant, item_id, qty_kth, 0, &mut leaves)?;
    let mut rows: Vec<(String, i64)> = leaves.into_iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows
        .into_iter()
        .map(|(item_id, qty_kth)| json!({"item_id": item_id, "qty_kth": qty_kth}))
        .collect())
}

/// One level of BOM requirements (components of `bom_id`), summed per component.
pub fn first_level_requirements(
    conn: &Connection,
    tenant: &str,
    bom_id: &str,
    qty_kth: i64,
) -> Res<Vec<(String, i64)>> {
    if qty_kth <= 0 {
        return Err("field 'qty_kth' must be greater than zero".to_string());
    }
    let header_scrap: i64 = conn
        .query_row(
            "SELECT scrap_kth FROM boms WHERE tenant_id = ?1 AND id = ?2",
            params![tenant, bom_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("bom '{}' not found", bom_id))?;
    let mut stmt = conn
        .prepare("SELECT component_id, qty_kth, scrap_kth FROM bom_lines WHERE tenant_id = ?1 AND bom_id = ?2 ORDER BY line_no ASC")
        .map_err(e)?;
    let lines = stmt
        .query_map(params![tenant, bom_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut totals: HashMap<String, i64> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (component, line_qty, line_scrap) in lines {
        let req = req_qty(qty_kth, line_qty, line_scrap, header_scrap)?;
        let entry = totals.entry(component.clone()).or_insert(0);
        if *entry == 0 && !order.contains(&component) {
            order.push(component);
        }
        *entry = entry
            .checked_add(req)
            .ok_or("quantity overflow in bom requirements")?;
    }
    Ok(order
        .into_iter()
        .filter_map(|c| totals.get(&c).map(|q| (c, *q)))
        .collect())
}

/// Creates a work order against the released BOM of an item. [manufacturing.wo.create]
pub fn create_work_order(
    conn: &Connection,
    ctx: &Ctx,
    item_id: &str,
    qty_kth: i64,
    warehouse_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.wo.create")?;
        if qty_kth <= 0 {
            return Err("field 'qty_kth' must be greater than zero".to_string());
        }
        let bom = released_bom_of(tx, &ctx.tenant, item_id)?
            .ok_or_else(|| format!("no released BOM exists for item '{}'", item_id))?;
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "WO", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO work_orders (tenant_id, id, number, item_id, bom_id, qty_kth, qty_output_kth, status, warehouse_id, created_by, created_at) VALUES (?1,?2,?3,?4,?5,?6,0,'created',?7,?8,?9)",
            params![ctx.tenant, id, number, item_id, bom.0, qty_kth, warehouse_id, ctx.user, now],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "work_order.created",
            &id,
            &json!({"number": number, "item_id": item_id, "bom_id": bom.0}),
            now,
        )?;
        Ok(
            json!({"id": id, "number": number, "item_id": item_id, "bom_id": bom.0, "status": "created"}),
        )
    })
}

/// Releases a created work order. [manufacturing.wo.release]
pub fn release_work_order(conn: &Connection, ctx: &Ctx, wo_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.wo.release")?;
        let status: String = tx
            .query_row(
                "SELECT status FROM work_orders WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, wo_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("work order '{}' not found", wo_id))?;
        if status != "created" {
            return Err(format!(
                "field 'status' must be 'created' to release, got '{}'",
                status
            ));
        }
        tx.execute(
            "UPDATE work_orders SET status = 'released' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, wo_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "work_order.released",
            wo_id,
            &json!({}),
            now,
        )?;
        Ok(json!({"id": wo_id, "status": "released"}))
    })
}

fn issued_cost_total(conn: &Connection, tenant: &str, wo_id: &str) -> Res<i64> {
    let mut stmt = conn
        .prepare("SELECT unit_cost_minor, qty_kth FROM work_order_issues WHERE tenant_id = ?1 AND wo_id = ?2")
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant, wo_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut total: i64 = 0;
    for (cost, qty) in rows {
        total = madd(total, mmul_qty(cost, qty)?)?;
    }
    Ok(total)
}

/// Issues first-level materials for the whole planned quantity of a work order. [manufacturing.wo.issue]
pub fn issue_wo_materials(
    conn: &Connection,
    ctx: &Ctx,
    wo_id: &str,
    ledger: &dyn Ledger,
    currency: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.wo.issue")?;
        let (bom_id, qty_kth, status, warehouse_id): (String, i64, String, String) = tx
            .query_row("SELECT bom_id, qty_kth, status, warehouse_id FROM work_orders WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, wo_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("work order '{}' not found", wo_id))?;
        if status != "released" && status != "in_progress" {
            return Err(format!(
                "field 'status' must be 'released' or 'in_progress' to issue materials, got '{}'",
                status
            ));
        }
        let already: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM work_order_issues WHERE tenant_id = ?1 AND wo_id = ?2",
                params![ctx.tenant, wo_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if already {
            return Err(format!(
                "materials already issued for work order '{}'",
                wo_id
            ));
        }
        let reqs = first_level_requirements(tx, &ctx.tenant, &bom_id, qty_kth)?;
        let mut total_issued: i64 = 0;
        for (item_id, req_qty) in &reqs {
            let outcome = inventory::issue_material(
                tx,
                ctx,
                item_id,
                &warehouse_id,
                *req_qty,
                "work_order",
                wo_id,
                now,
            )?;
            let unit_cost_i128 =
                (outcome.total_cost_minor as i128) * 1000i128 + (*req_qty as i128) / 2i128;
            let unit_cost = i64::try_from(unit_cost_i128 / (*req_qty as i128)).map_err(e)?;
            tx.execute(
                "INSERT INTO work_order_issues (tenant_id, id, wo_id, item_id, qty_kth, unit_cost_minor, warehouse_id, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![ctx.tenant, new_id(), wo_id, item_id, req_qty, unit_cost, warehouse_id, now],
            )
            .map_err(e)?;
            total_issued = madd(total_issued, outcome.total_cost_minor)?;
        }
        tx.execute(
            "UPDATE work_orders SET status = 'in_progress' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, wo_id],
        )
        .map_err(e)?;
        if total_issued > 0 {
            ledger.post(
                &ctx.tenant,
                &JournalEntry {
                    date: today(now),
                    memo: format!("Work order {} material issue", wo_id),
                    currency: currency.to_string(),
                    lines: vec![
                        JournalLine {
                            account: "1400".to_string(),
                            debit: total_issued,
                            credit: 0,
                        },
                        JournalLine {
                            account: "1300".to_string(),
                            debit: 0,
                            credit: total_issued,
                        },
                    ],
                },
            )?;
        }
        audit(
            tx,
            &ctx.tenant,
            "work_order.materials_issued",
            wo_id,
            &json!({"total_issued_minor": total_issued}),
            now,
        )?;
        Ok(
            json!({"id": wo_id, "status": "in_progress", "total_issued_minor": total_issued, "issues": reqs.len()}),
        )
    })
}

/// Reports output quantity for an in-progress work order (max 105% of planned cumulative). [manufacturing.wo.output]
pub fn report_output(
    conn: &Connection,
    ctx: &Ctx,
    wo_id: &str,
    qty_kth: i64,
    ledger: &dyn Ledger,
    currency: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.wo.output")?;
        if qty_kth <= 0 {
            return Err("field 'qty_kth' must be greater than zero".to_string());
        }
        let (item_id, qty_kth_planned, qty_output, status, warehouse_id): (String, i64, i64, String, String) = tx
            .query_row("SELECT item_id, qty_kth, qty_output_kth, status, warehouse_id FROM work_orders WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, wo_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("work order '{}' not found", wo_id))?;
        if status != "in_progress" {
            return Err(format!(
                "field 'status' must be 'in_progress' to report output, got '{}'",
                status
            ));
        }
        let new_output = qadd(qty_output, qty_kth)?;
        let max_allowed = (qty_kth_planned as i128) * 105 / 100;
        if (new_output as i128) > max_allowed {
            return Err(format!("field 'qty_kth' exceeds 105% of planned quantity: planned {} thousandths, already reported {}, requested additional {}", qty_kth_planned, qty_output, qty_kth));
        }
        let total_issued = issued_cost_total(tx, &ctx.tenant, wo_id)?;
        let unit_cost_i128 = (total_issued as i128) * 1000i128 + (qty_kth_planned as i128) / 2i128;
        let unit_cost = i64::try_from(unit_cost_i128 / (qty_kth_planned as i128)).map_err(e)?;
        inventory::receive_material(
            tx,
            ctx,
            &inventory::ReceiveParams {
                item_id: item_id.clone(),
                warehouse_id: warehouse_id.clone(),
                qty_kth,
                unit_cost_minor: unit_cost,
                batch_no: None,
                batch_expiry: None,
                ref_type: Some("work_order".to_string()),
                ref_id: Some(wo_id.to_string()),
            },
            now,
        )?;
        tx.execute(
            "INSERT INTO work_order_outputs (tenant_id, id, wo_id, item_id, qty_kth, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![ctx.tenant, new_id(), wo_id, item_id, qty_kth, now],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE work_orders SET qty_output_kth = ?3 WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, wo_id, new_output],
        )
        .map_err(e)?;
        let output_value = mmul_qty(unit_cost, qty_kth)?;
        if output_value > 0 {
            ledger.post(
                &ctx.tenant,
                &JournalEntry {
                    date: today(now),
                    memo: format!("Work order {} output", wo_id),
                    currency: currency.to_string(),
                    lines: vec![
                        JournalLine {
                            account: "1300".to_string(),
                            debit: output_value,
                            credit: 0,
                        },
                        JournalLine {
                            account: "1400".to_string(),
                            debit: 0,
                            credit: output_value,
                        },
                    ],
                },
            )?;
        }
        audit(
            tx,
            &ctx.tenant,
            "work_order.output_reported",
            wo_id,
            &json!({"qty_kth": qty_kth, "unit_cost_minor": unit_cost}),
            now,
        )?;
        Ok(
            json!({"id": wo_id, "qty_output_kth": new_output, "unit_cost_minor": unit_cost, "output_value_minor": output_value}),
        )
    })
}

/// Closes a work order and posts manufacturing variance (Dr 5200 / Cr 1400 when positive, reversed when negative). [manufacturing.wo.close]
pub fn close_work_order(
    conn: &Connection,
    ctx: &Ctx,
    wo_id: &str,
    ledger: &dyn Ledger,
    currency: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.wo.close")?;
        let (qty_kth_planned, status): (i64, String) = tx
            .query_row(
                "SELECT qty_kth, status FROM work_orders WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, wo_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("work order '{}' not found", wo_id))?;
        if status == "closed" {
            return Err(format!("work order '{}' is already closed", wo_id));
        }
        let total_issued = issued_cost_total(tx, &ctx.tenant, wo_id)?;
        let unit_cost = if qty_kth_planned > 0 {
            let v = (total_issued as i128) * 1000i128 + (qty_kth_planned as i128) / 2i128;
            i64::try_from(v / (qty_kth_planned as i128)).map_err(e)?
        } else {
            0
        };
        let mut absorbed: i64 = 0;
        {
            let mut stmt = tx
                .prepare(
                    "SELECT qty_kth FROM work_order_outputs WHERE tenant_id = ?1 AND wo_id = ?2",
                )
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant, wo_id], |r| r.get::<_, i64>(0))
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            for qty in rows {
                absorbed = madd(absorbed, mmul_qty(unit_cost, qty)?)?;
            }
        }
        let variance = msub(total_issued, absorbed)?;
        if variance != 0 {
            let amount = variance.saturating_abs();
            let lines = if variance > 0 {
                vec![
                    JournalLine {
                        account: "5200".to_string(),
                        debit: amount,
                        credit: 0,
                    },
                    JournalLine {
                        account: "1400".to_string(),
                        debit: 0,
                        credit: amount,
                    },
                ]
            } else {
                vec![
                    JournalLine {
                        account: "1400".to_string(),
                        debit: amount,
                        credit: 0,
                    },
                    JournalLine {
                        account: "5200".to_string(),
                        debit: 0,
                        credit: amount,
                    },
                ]
            };
            ledger.post(
                &ctx.tenant,
                &JournalEntry {
                    date: today(now),
                    memo: format!("Work order {} variance", wo_id),
                    currency: currency.to_string(),
                    lines,
                },
            )?;
        }
        tx.execute("UPDATE work_orders SET status = 'closed', closed_at = ?3 WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, wo_id, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "work_order.closed",
            wo_id,
            &json!({"variance_minor": variance}),
            now,
        )?;
        Ok(json!({"id": wo_id, "status": "closed", "variance_minor": variance}))
    })
}

fn open_po_qty_for_item(lines: &[Value], item_id: &str) -> Res<i64> {
    let mut total: i64 = 0;
    for line in lines {
        if line.get("item_id").and_then(|v| v.as_str()) == Some(item_id) {
            let q = jint(line, "qty_open_kth")?;
            total = qadd(total, q)?;
        }
    }
    Ok(total)
}

fn planned_output_supply(conn: &Connection, tenant: &str, item_id: &str) -> Res<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(MAX(0, qty_kth - qty_output_kth)), 0) FROM work_orders WHERE tenant_id = ?1 AND item_id = ?2 AND status != 'closed'",
        params![tenant, item_id],
        |r| r.get(0),
    )
    .map_err(e)
}

/// Runs MRP read-only and returns purchase / work order proposals sorted by item code then kind. [manufacturing.mrp.run]
pub fn mrp_run(conn: &Connection, ctx: &Ctx, as_of: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.mrp.run")?;
        let mut demand: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = tx
                .prepare("SELECT l.item_id, COALESCE(SUM(l.qty_kth - l.qty_delivered_kth), 0) FROM sales_order_lines l JOIN sales_orders o ON o.tenant_id = l.tenant_id AND o.id = l.order_id WHERE o.tenant_id = ?1 AND o.status IN ('open','partially_delivered') GROUP BY l.item_id")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            for (item, qty) in rows {
                if qty > 0 {
                    *demand.entry(item).or_insert(0) = qty;
                }
            }
        }
        let mut item_meta: HashMap<String, (String, i64, i64)> = HashMap::new();
        {
            let mut stmt = tx
                .prepare("SELECT id, code, lead_time_days, safety_stock_kth FROM items WHERE tenant_id = ?1 AND active = 1")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant], |r| {
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
            for (id, code, lead, safety) in rows {
                if safety > 0 {
                    let entry = demand.entry(id.clone()).or_insert(0);
                    *entry = entry.checked_add(safety).ok_or("demand overflow")?;
                }
                item_meta.insert(id, (code, lead, safety));
            }
        }
        let po_lines = procurement::open_po_lines(tx, &ctx.tenant)?;

        fn supply_of(
            conn: &Connection,
            tenant: &str,
            item_id: &str,
            po_lines: &[Value],
        ) -> Res<i64> {
            let on_hand = inventory::on_hand_total(conn, tenant, item_id)?;
            let po_qty = open_po_qty_for_item(po_lines, item_id)?;
            let planned = planned_output_supply(conn, tenant, item_id)?;
            on_hand
                .checked_add(po_qty)
                .and_then(|v| v.checked_add(planned))
                .ok_or_else(|| "supply arithmetic overflow".to_string())
        }

        // Expand demand layer by layer: each pass sums demand first, then adds
        // first-level requirements of items that still have net demand and a released BOM.
        let mut requirements_added: HashSet<String> = HashSet::new();
        for _pass in 0..20 {
            let snapshot: Vec<(String, i64)> =
                demand.iter().map(|(k, v)| (k.clone(), *v)).collect();
            let mut additions: HashMap<String, i64> = HashMap::new();
            for (item, dem) in snapshot {
                if dem <= 0 {
                    continue;
                }
                let supply = supply_of(tx, &ctx.tenant, &item, &po_lines)?;
                let net = dem.saturating_sub(supply);
                if net <= 0 || requirements_added.contains(&item) {
                    continue;
                }
                if let Some((bom_id, _)) = released_bom_of(tx, &ctx.tenant, &item)? {
                    let reqs = first_level_requirements(tx, &ctx.tenant, &bom_id, net)?;
                    for (comp, qty) in reqs {
                        let entry = additions.entry(comp).or_insert(0);
                        *entry = entry
                            .checked_add(qty)
                            .ok_or("requirement addition overflow")?;
                    }
                    requirements_added.insert(item);
                }
            }
            if additions.is_empty() {
                break;
            }
            for (k, v) in additions {
                let entry = demand.entry(k).or_insert(0);
                *entry = entry.checked_add(v).ok_or("demand overflow")?;
            }
        }

        let mut proposals: Vec<(String, String, String, i64, i64)> = Vec::new(); // code, item_id, kind, qty, order_by
        for (item, dem) in &demand {
            if *dem <= 0 {
                continue;
            }
            let supply = supply_of(tx, &ctx.tenant, item, &po_lines)?;
            let net = dem.saturating_sub(supply);
            if net <= 0 {
                continue;
            }
            let (code, lead) = match item_meta.get(item) {
                Some((c, l, _)) => (c.clone(), *l),
                None => {
                    let row: Option<(String, i64)> = tx
                        .query_row("SELECT code, lead_time_days FROM items WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, item], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()
                        .map_err(e)?;
                    match row {
                        Some((c, l)) => (c, l),
                        None => continue,
                    }
                }
            };
            let kind = if released_bom_of(tx, &ctx.tenant, item)?.is_some() {
                "work_order"
            } else {
                "purchase"
            };
            let lead_secs = lead
                .checked_mul(86_400)
                .ok_or("lead time arithmetic overflow")?;
            let order_by = as_of
                .checked_sub(lead_secs)
                .ok_or("order_by arithmetic overflow")?;
            proposals.push((code, item.clone(), kind.to_string(), net, order_by));
        }
        proposals.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
        let arr: Vec<Value> = proposals
            .into_iter()
            .map(|(_, item_id, kind, qty, order_by)| json!({"kind": kind, "item_id": item_id, "qty_kth": qty, "order_by": order_by, "due": as_of}))
            .collect();
        audit(
            tx,
            &ctx.tenant,
            "mrp.run",
            "mrp",
            &json!({"proposals": arr.len(), "as_of": as_of}),
            as_of,
        )?;
        Ok(json!({"proposals": arr}))
    })
}

/// Creates a quality inspection; `fail`/`hold` with `hold_qty_kth > 0` reserves the stock open. [manufacturing.quality.inspect]
pub fn create_inspection(
    conn: &Connection,
    ctx: &Ctx,
    subject_type: &str,
    subject_id: &str,
    item_id: &str,
    sample_size_kth: i64,
    result: &str,
    hold_qty_kth: i64,
    warehouse_id: &str,
    notes: Option<&str>,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.quality.inspect")?;
        match result {
            "pass" | "fail" | "hold" => {}
            other => {
                return Err(format!(
                    "field 'result' must be 'pass', 'fail' or 'hold', got '{}'",
                    other
                ))
            }
        }
        if sample_size_kth <= 0 {
            return Err("field 'sample_size_kth' must be greater than zero".to_string());
        }
        if hold_qty_kth < 0 {
            return Err("field 'hold_qty_kth' must be greater than or equal to zero".to_string());
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO inspections (tenant_id, id, subject_type, subject_id, item_id, sample_size_kth, result, notes, created_by, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![ctx.tenant, id, subject_type, subject_id, item_id, sample_size_kth, result, notes, ctx.user, now],
        )
        .map_err(e)?;
        if (result == "fail" || result == "hold") && hold_qty_kth > 0 {
            let on_hand = inventory::on_hand(tx, &ctx.tenant, item_id, warehouse_id)?;
            let reserved = inventory::reserved_qty(tx, &ctx.tenant, item_id, warehouse_id)?;
            let available = on_hand
                .checked_sub(reserved)
                .ok_or("availability arithmetic underflow")?;
            if available < hold_qty_kth {
                return Err(format!("field 'hold_qty_kth' exceeds available stock for item '{}' in warehouse '{}': available {} thousandths, requested {}", item_id, warehouse_id, available, hold_qty_kth));
            }
            tx.execute(
                "INSERT INTO reservations (tenant_id, id, item_id, warehouse_id, qty_kth, ref_type, ref_id, status, created_at) VALUES (?1,?2,?3,?4,?5,'quality_hold',?6,'open',?7)",
                params![ctx.tenant, new_id(), item_id, warehouse_id, hold_qty_kth, id, now],
            )
            .map_err(e)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "quality.inspection_created",
            &id,
            &json!({"result": result, "hold_qty_kth": hold_qty_kth}),
            now,
        )?;
        Ok(
            json!({"id": id, "subject_type": subject_type, "subject_id": subject_id, "result": result, "hold_qty_kth": hold_qty_kth}),
        )
    })
}

/// Releases all open quality-hold reservations linked to an inspection. [manufacturing.quality.release]
pub fn release_quality_hold(
    conn: &Connection,
    ctx: &Ctx,
    inspection_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "manufacturing.quality.release")?;
        let exists: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM inspections WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, inspection_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !exists {
            return Err(format!("inspection '{}' not found", inspection_id));
        }
        let n = tx
            .execute("UPDATE reservations SET status = 'released' WHERE tenant_id = ?1 AND ref_type = 'quality_hold' AND ref_id = ?2 AND status = 'open'", params![ctx.tenant, inspection_id])
            .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "quality.hold_released",
            inspection_id,
            &json!({"released": n}),
            now,
        )?;
        Ok(json!({"id": inspection_id, "released": n}))
    })
}
