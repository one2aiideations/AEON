//! Items, movements, FIFO and weighted-average valuation, reservations, serial numbers and cycle counts.

use crate::common::{
    audit, e, in_tx, mmul_qty, new_id, require_perm, ymd, Ctx, JournalEntry, JournalLine, Ledger,
    Res,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

/// Parameters to create an item.
pub struct CreateItem {
    /// Business code, unique per tenant.
    pub code: String,
    /// Display name.
    pub name: String,
    /// Base unit of measure.
    pub uom: String,
    /// 'FIFO' or 'WA'.
    pub valuation_method: String,
    /// Reorder point in thousandths of the base UoM.
    pub reorder_point_kth: i64,
    /// Allow stock to go negative on issue/adjustment.
    pub allows_negative: bool,
    /// Opening weighted-average cost in minor units (used by WA).
    pub initial_cost_minor: i64,
}

/// Parameters for a stock receipt.
pub struct ReceiveParams {
    /// Item to receive.
    pub item_id: String,
    /// Destination warehouse.
    pub warehouse_id: String,
    /// Quantity in thousandths; must be > 0.
    pub qty_kth: i64,
    /// Unit cost in minor units; >= 0.
    pub unit_cost_minor: i64,
    /// Optional batch number.
    pub batch_no: Option<String>,
    /// Optional batch expiry, YYYYMMDD (required valid if batch_no set with expiry).
    pub batch_expiry: Option<String>,
    /// Optional source reference type, e.g. 'purchase_order'.
    pub ref_type: Option<String>,
    /// Optional source reference id.
    pub ref_id: Option<String>,
}

/// Parameters for a stock issue.
pub struct IssueParams {
    /// Item to issue.
    pub item_id: String,
    /// Source warehouse.
    pub warehouse_id: String,
    /// Quantity in thousandths; must be > 0.
    pub qty_kth: i64,
    /// Reference type, e.g. 'sales_order', 'work_order'.
    pub ref_type: String,
    /// Reference id.
    pub ref_id: String,
}

/// Parameters for a warehouse transfer.
pub struct TransferParams {
    /// Item to move.
    pub item_id: String,
    /// Source warehouse.
    pub from_wh: String,
    /// Destination warehouse.
    pub to_wh: String,
    /// Quantity in thousandths; must be > 0.
    pub qty_kth: i64,
}

/// Outcome of a stock issue.
pub struct IssueOutcome {
    /// Movement row id.
    pub movement_id: String,
    /// Total cost of the issued quantity in minor units.
    pub total_cost_minor: i64,
}

struct ItemRow {
    method: String,
    wa_cost: i64,
    allows_neg: bool,
    active: i64,
}

fn item_row(conn: &rusqlite::Connection, tenant: &str, item_id: &str) -> Res<ItemRow> {
    conn.query_row(
        "SELECT valuation_method, weighted_avg_cost_minor, allows_negative, active FROM items WHERE tenant_id=?1 AND id=?2",
        params![tenant, item_id],
        |r| Ok(ItemRow { method: r.get(0)?, wa_cost: r.get(1)?, allows_neg: r.get::<_, i64>(2)? != 0, active: r.get(3)? }),
    ).optional().map_err(e)?.ok_or_else(|| format!("item '{item_id}' not found"))
}

/// Quantity on hand (signed sum of movements) for one warehouse.
pub fn on_hand(
    conn: &rusqlite::Connection,
    tenant: &str,
    item_id: &str,
    warehouse_id: &str,
) -> Res<i64> {
    conn.query_row("SELECT COALESCE(SUM(qty_kth),0) FROM stock_movements WHERE tenant_id=?1 AND item_id=?2 AND warehouse_id=?3",
        params![tenant, item_id, warehouse_id], |r| r.get(0)).map_err(e)
}

/// Total on hand across all warehouses.
pub fn on_hand_total(conn: &rusqlite::Connection, tenant: &str, item_id: &str) -> Res<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(qty_kth),0) FROM stock_movements WHERE tenant_id=?1 AND item_id=?2",
        params![tenant, item_id],
        |r| r.get(0),
    )
    .map_err(e)
}

/// Quantity reserved (open reservations) for one warehouse.
pub fn reserved_qty(
    conn: &rusqlite::Connection,
    tenant: &str,
    item_id: &str,
    warehouse_id: &str,
) -> Res<i64> {
    conn.query_row("SELECT COALESCE(SUM(qty_kth),0) FROM reservations WHERE tenant_id=?1 AND item_id=?2 AND warehouse_id=?3 AND status='open'",
        params![tenant, item_id, warehouse_id], |r| r.get(0)).map_err(e)
}

fn next_movement_seq(conn: &rusqlite::Connection, tenant: &str) -> Res<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(movement_seq),0)+1 FROM stock_movements WHERE tenant_id=?1",
        params![tenant],
        |r| r.get(0),
    )
    .map_err(e)
}

struct MovementIn {
    item_id: String,
    warehouse_id: String,
    bin_id: Option<String>,
    batch_no: Option<String>,
    mtype: &'static str,
    qty_kth: i64,
    unit_cost_minor: i64,
    ref_type: Option<String>,
    ref_id: Option<String>,
}

fn insert_movement(
    conn: &rusqlite::Connection,
    tenant: &str,
    m: MovementIn,
    user: &str,
    now: i64,
) -> Res<String> {
    let seq = next_movement_seq(conn, tenant)?;
    let id = new_id();
    conn.execute("INSERT INTO stock_movements(tenant_id,id,movement_seq,item_id,warehouse_id,bin_id,batch_no,movement_type,qty_kth,unit_cost_minor,ref_type,ref_id,created_by,created_at) \
                  VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![tenant, id, seq, m.item_id, m.warehouse_id, m.bin_id, m.batch_no, m.mtype, m.qty_kth, m.unit_cost_minor, m.ref_type, m.ref_id, user, now]).map_err(e)?;
    Ok(id)
}

fn wa_recompute(
    conn: &rusqlite::Connection,
    tenant: &str,
    item_id: &str,
    add_qty: i64,
    add_cost: i64,
) -> Res<()> {
    let (avg,): (i64,) = conn
        .query_row(
            "SELECT weighted_avg_cost_minor FROM items WHERE tenant_id=?1 AND id=?2",
            params![tenant, item_id],
            |r| Ok((r.get(0)?,)),
        )
        .map_err(e)?;
    let before = on_hand_total(conn, tenant, item_id)?;
    let base = if before > 0 { before } else { 0 };
    let new_qty = base
        .checked_add(add_qty)
        .ok_or_else(|| "quantity overflow in weighted average".to_string())?;
    let new_avg = if new_qty <= 0 {
        add_cost
    } else {
        let num = (base as i128) * (avg as i128) + (add_qty as i128) * (add_cost as i128);
        i64::try_from(num / (new_qty as i128))
            .map_err(|_| "weighted average cost overflow".to_string())?
    };
    conn.execute(
        "UPDATE items SET weighted_avg_cost_minor=?1 WHERE tenant_id=?2 AND id=?3",
        params![new_avg, tenant, item_id],
    )
    .map_err(e)?;
    Ok(())
}

fn fifo_push(
    conn: &rusqlite::Connection,
    tenant: &str,
    item_id: &str,
    wh: &str,
    batch: &Option<String>,
    qty: i64,
    cost: i64,
    now: i64,
) -> Res<()> {
    conn.execute("INSERT INTO fifo_lots(tenant_id,id,item_id,warehouse_id,batch_no,qty_remaining_kth,unit_cost_minor,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![tenant, new_id(), item_id, wh, batch, qty, cost, now]).map_err(e)?;
    Ok(())
}

fn fifo_consume(
    conn: &rusqlite::Connection,
    tenant: &str,
    item_id: &str,
    wh: &str,
    qty: i64,
    wa_cost: i64,
    allows_neg: bool,
) -> Res<i64> {
    let mut stmt = conn.prepare("SELECT id, qty_remaining_kth, unit_cost_minor FROM fifo_lots WHERE tenant_id=?1 AND item_id=?2 AND warehouse_id=?3 AND qty_remaining_kth>0 ORDER BY created_at, id").map_err(e)?;
    let lots = stmt
        .query_map(params![tenant, item_id, wh], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut need = qty;
    let mut total: i128 = 0;
    for (id, rem, cost) in lots {
        if need == 0 {
            break;
        }
        let take = rem.min(need);
        conn.execute("UPDATE fifo_lots SET qty_remaining_kth = qty_remaining_kth - ?1 WHERE tenant_id=?2 AND id=?3", params![take, tenant, id]).map_err(e)?;
        need -= take;
        total += (take as i128) * (cost as i128) / 1000;
    }
    if need > 0 {
        if allows_neg {
            total += (need as i128) * (wa_cost as i128) / 1000;
        } else {
            return Err(format!("insufficient FIFO cost layers for item '{item_id}' in warehouse '{wh}' (short {} thousandths)", need));
        }
    }
    i64::try_from(total).map_err(|_| "FIFO cost overflow".to_string())
}

/// Create an item. Validates code/name/UoM and the valuation method.
pub fn create_item(conn: &rusqlite::Connection, ctx: &Ctx, p: &CreateItem, now: i64) -> Res<Value> {
    let payload = json!({"code": p.code, "name": p.name});
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.items.create")?;
        let code = p.code.trim().to_string();
        if code.is_empty() {
            return Err("item code must not be empty".to_string());
        }
        if p.name.trim().is_empty() {
            return Err("item name must not be empty".to_string());
        }
        if p.uom.trim().is_empty() {
            return Err("item uom must not be empty".to_string());
        }
        if p.valuation_method != "FIFO" && p.valuation_method != "WA" {
            return Err("valuation_method must be 'FIFO' or 'WA'".to_string());
        }
        if p.reorder_point_kth < 0 {
            return Err("reorder_point_kth must not be negative".to_string());
        }
        if p.initial_cost_minor < 0 {
            return Err("initial_cost_minor must not be negative".to_string());
        }
        let id = new_id();
        tx.execute("INSERT INTO items(tenant_id,id,code,name,uom,valuation_method,weighted_avg_cost_minor,reorder_point_kth,allows_negative,active) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1)",
            params![ctx.tenant, id, code, p.name.trim(), p.uom.trim().to_uppercase(), p.valuation_method, p.initial_cost_minor, p.reorder_point_kth, p.allows_negative as i64])
            .map_err(|err| format!("item code '{code}' cannot be created (duplicate?): {err}"))?;
        audit(tx, &ctx.tenant, "item.create", &id, &payload, now)?;
        Ok(json!({"id": id, "code": code}))
    })
}

/// Receive stock inside a caller's transaction: no permission check and no transaction of its own
/// (callers enforce the permission). Writes the movement, valuation layer, batch and audit row.
pub fn receive_material(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &ReceiveParams,
    now: i64,
) -> Res<Value> {
    if p.qty_kth <= 0 {
        return Err("quantity received must be greater than zero (field 'qty_kth')".to_string());
    }
    if p.unit_cost_minor < 0 {
        return Err("unit cost must not be negative (field 'unit_cost_minor')".to_string());
    }
    if let Some(exp) = &p.batch_expiry {
        let ok = exp.len() == 8 && exp.bytes().all(|b| b.is_ascii_digit());
        if !ok {
            return Err("batch_expiry must be YYYYMMDD (8 ASCII digits)".to_string());
        }
    }
    let item = item_row(conn, &ctx.tenant, &p.item_id)?;
    if item.active == 0 {
        return Err(format!("item '{}' is inactive", p.item_id));
    }
    // The weighted average must see the stock as it was BEFORE this receipt is added.
    if item.method == "WA" {
        wa_recompute(conn, &ctx.tenant, &p.item_id, p.qty_kth, p.unit_cost_minor)?;
    }
    let mid = insert_movement(
        conn,
        &ctx.tenant,
        MovementIn {
            item_id: p.item_id.clone(),
            warehouse_id: p.warehouse_id.clone(),
            bin_id: None,
            batch_no: p.batch_no.clone(),
            mtype: if p.ref_type.as_deref() == Some("return") {
                "return"
            } else {
                "receipt"
            },
            qty_kth: p.qty_kth,
            unit_cost_minor: p.unit_cost_minor,
            ref_type: p.ref_type.clone(),
            ref_id: p.ref_id.clone(),
        },
        &ctx.user,
        now,
    )?;
    if item.method != "WA" {
        fifo_push(
            conn,
            &ctx.tenant,
            &p.item_id,
            &p.warehouse_id,
            &p.batch_no,
            p.qty_kth,
            p.unit_cost_minor,
            now,
        )?;
    }
    if let (Some(b), Some(exp)) = (&p.batch_no, &p.batch_expiry) {
        conn.execute("INSERT INTO batches(tenant_id,item_id,batch_no,expiry_yyyymmdd) VALUES (?1,?2,?3,?4) ON CONFLICT(tenant_id,item_id,batch_no) DO NOTHING",
            params![ctx.tenant, p.item_id, b, exp]).map_err(e)?;
    }
    audit(
        conn,
        &ctx.tenant,
        "stock.receipt",
        &mid,
        &json!({"item_id": p.item_id, "warehouse_id": p.warehouse_id, "qty_kth": p.qty_kth, "unit_cost_minor": p.unit_cost_minor}),
        now,
    )?;
    Ok(json!({"movement_id": mid, "qty_kth": p.qty_kth}))
}

/// Receive stock (purchase receipt, return, opening balance) with permission check and its own transaction.
pub fn receive_stock(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &ReceiveParams,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.receive")?;
        receive_material(tx, ctx, p, now)
    })
}

/// Issue stock; internal helper used by sales/manufacturing inside their transactions.
/// Performs the negative-stock check and valuation. No permission check; callers enforce it.
pub fn issue_material(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    item_id: &str,
    warehouse_id: &str,
    qty_kth: i64,
    ref_type: &str,
    ref_id: &str,
    now: i64,
) -> Res<IssueOutcome> {
    if qty_kth <= 0 {
        return Err("quantity issued must be greater than zero".to_string());
    }
    let item = item_row(conn, &ctx.tenant, item_id)?;
    if item.active == 0 {
        return Err(format!("item '{item_id}' is inactive"));
    }
    let have = on_hand(conn, &ctx.tenant, item_id, warehouse_id)?;
    if !item.allows_neg
        && have
            .checked_sub(qty_kth)
            .ok_or_else(|| "quantity overflow".to_string())?
            < 0
    {
        return Err(format!(
            "insufficient stock for item '{}' in warehouse '{}' (have {}.{:03}, need {}.{:03})",
            item_id,
            warehouse_id,
            have / 1000,
            (have % 1000).abs(),
            qty_kth / 1000,
            (qty_kth % 1000).abs()
        ));
    }
    let total_cost = match item.method.as_str() {
        "FIFO" => fifo_consume(
            conn,
            &ctx.tenant,
            item_id,
            warehouse_id,
            qty_kth,
            item.wa_cost,
            item.allows_neg,
        )?,
        _ => mmul_qty(item.wa_cost, qty_kth)?,
    };
    let unit = i64::try_from((total_cost as i128) * 1000 / (qty_kth as i128))
        .map_err(|_| "unit cost overflow".to_string())?;
    let mid = insert_movement(
        conn,
        &ctx.tenant,
        MovementIn {
            item_id: item_id.to_string(),
            warehouse_id: warehouse_id.to_string(),
            bin_id: None,
            batch_no: None,
            mtype: "issue",
            qty_kth: -qty_kth,
            unit_cost_minor: unit,
            ref_type: Some(ref_type.to_string()),
            ref_id: Some(ref_id.to_string()),
        },
        &ctx.user,
        now,
    )?;
    Ok(IssueOutcome {
        movement_id: mid,
        total_cost_minor: total_cost,
    })
}

/// Issue stock with permission check, transaction and audit. Returns movement id and cost.
pub fn issue_stock(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &IssueParams,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.issue")?;
        let out = issue_material(
            tx,
            ctx,
            &p.item_id,
            &p.warehouse_id,
            p.qty_kth,
            &p.ref_type,
            &p.ref_id,
            now,
        )?;
        audit(
            tx,
            &ctx.tenant,
            "stock.issue",
            &out.movement_id,
            &json!({"item_id": p.item_id, "warehouse_id": p.warehouse_id, "qty_kth": p.qty_kth, "total_cost_minor": out.total_cost_minor}),
            now,
        )?;
        Ok(json!({"movement_id": out.movement_id, "total_cost_minor": out.total_cost_minor}))
    })
}

/// Signed stock adjustment (cycle count postings, manual corrections). Negative respects allows_negative.
pub fn adjust_stock(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    item_id: &str,
    warehouse_id: &str,
    delta_kth: i64,
    reason: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.adjust")?;
        if delta_kth == 0 {
            return Err("adjustment delta must not be zero".to_string());
        }
        if reason.trim().is_empty() {
            return Err("adjustment reason must not be empty (field 'reason')".to_string());
        }
        let item = item_row(tx, &ctx.tenant, item_id)?;
        if delta_kth < 0 {
            let have = on_hand(tx, &ctx.tenant, item_id, warehouse_id)?;
            if !item.allows_neg
                && have
                    .checked_add(delta_kth)
                    .ok_or_else(|| "quantity overflow".to_string())?
                    < 0
            {
                return Err(format!(
                    "adjustment would drive stock negative for item '{item_id}'"
                ));
            }
            fifo_consume(
                tx,
                &ctx.tenant,
                item_id,
                warehouse_id,
                -delta_kth,
                item.wa_cost,
                item.allows_neg,
            )?;
        } else if item.method == "FIFO" {
            fifo_push(
                tx,
                &ctx.tenant,
                item_id,
                warehouse_id,
                &None,
                delta_kth,
                item.wa_cost,
                now,
            )?;
        }
        let mid = insert_movement(
            tx,
            &ctx.tenant,
            MovementIn {
                item_id: item_id.to_string(),
                warehouse_id: warehouse_id.to_string(),
                bin_id: None,
                batch_no: None,
                mtype: "adjustment",
                qty_kth: delta_kth,
                unit_cost_minor: item.wa_cost,
                ref_type: Some("adjustment".to_string()),
                ref_id: Some(reason.to_string()),
            },
            &ctx.user,
            now,
        )?;
        audit(
            tx,
            &ctx.tenant,
            "stock.adjustment",
            &mid,
            &json!({"item_id": item_id, "warehouse_id": warehouse_id, "delta_kth": delta_kth, "reason": reason}),
            now,
        )?;
        Ok(json!({"movement_id": mid, "delta_kth": delta_kth}))
    })
}

/// Transfer stock between warehouses, preserving ledger value.
pub fn transfer_stock(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &TransferParams,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.transfer")?;
        if p.qty_kth <= 0 {
            return Err("transfer quantity must be greater than zero".to_string());
        }
        if p.from_wh == p.to_wh {
            return Err("transfer source and destination warehouses must differ".to_string());
        }
        let out = issue_material(
            tx, ctx, &p.item_id, &p.from_wh, p.qty_kth, "transfer", &p.to_wh, now,
        )?;
        let unit = i64::try_from((out.total_cost_minor as i128) * 1000 / (p.qty_kth as i128))
            .map_err(|_| "unit cost overflow".to_string())?;
        let item = item_row(tx, &ctx.tenant, &p.item_id)?;
        if item.method == "FIFO" {
            fifo_push(
                tx,
                &ctx.tenant,
                &p.item_id,
                &p.to_wh,
                &None,
                p.qty_kth,
                unit,
                now,
            )?;
        }
        let mid = insert_movement(
            tx,
            &ctx.tenant,
            MovementIn {
                item_id: p.item_id.clone(),
                warehouse_id: p.to_wh.clone(),
                bin_id: None,
                batch_no: None,
                mtype: "transfer_in",
                qty_kth: p.qty_kth,
                unit_cost_minor: unit,
                ref_type: Some("transfer".to_string()),
                ref_id: Some(p.from_wh.clone()),
            },
            &ctx.user,
            now,
        )?;
        // Rewrite the out movement type for symmetry.
        tx.execute(
            "UPDATE stock_movements SET movement_type='transfer_out' WHERE tenant_id=?1 AND id=?2",
            params![ctx.tenant, out.movement_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "stock.transfer",
            &mid,
            &json!({"item_id": p.item_id, "from": p.from_wh, "to": p.to_wh, "qty_kth": p.qty_kth, "value_minor": out.total_cost_minor}),
            now,
        )?;
        Ok(
            json!({"transfer_in": mid, "transfer_out": out.movement_id, "value_minor": out.total_cost_minor}),
        )
    })
}

/// Stock value of a warehouse scope (or company-wide): FIFO lots at cost, WA at average.
pub fn stock_value_minor(
    conn: &rusqlite::Connection,
    tenant: &str,
    warehouse_id: Option<&str>,
) -> Res<i64> {
    let fifo: i64 = conn.query_row("SELECT COALESCE(SUM(qty_remaining_kth * unit_cost_minor / 1000),0) FROM fifo_lots WHERE tenant_id=?1 AND (?2 IS NULL OR warehouse_id=?2)",
        params![tenant, warehouse_id], |r| r.get(0)).map_err(e)?;
    // WA items that never used FIFO lots: company-wide qty minus FIFO-tracked qty would double count,
    // so value WA items as sum over WA items of on_hand*avg (no FIFO lots exist for them).
    let wa: i64 = conn.query_row(
        "SELECT COALESCE(SUM((SELECT COALESCE(SUM(m.qty_kth),0) FROM stock_movements m WHERE m.tenant_id=items.tenant_id AND m.item_id=items.id AND (?2 IS NULL OR m.warehouse_id=?2)) * items.weighted_avg_cost_minor / 1000),0) \
         FROM items WHERE tenant_id=?1 AND valuation_method='WA'",
        params![tenant, warehouse_id], |r| r.get(0)).map_err(e)?;
    Ok(fifo.saturating_add(wa))
}

/// Stock-on-hand rows, optionally filtered by item and warehouse. No permission check.
pub fn stock_on_hand_rows(
    conn: &rusqlite::Connection,
    tenant: &str,
    item_id: Option<&str>,
    warehouse_id: Option<&str>,
) -> Res<Vec<Value>> {
    let mut stmt = conn
        .prepare(
            "SELECT item_id, warehouse_id, COALESCE(SUM(qty_kth),0) AS qty FROM stock_movements \
         WHERE tenant_id=?1 AND (?2 IS NULL OR item_id=?2) AND (?3 IS NULL OR warehouse_id=?3) \
         GROUP BY item_id, warehouse_id HAVING qty != 0 ORDER BY item_id, warehouse_id",
        )
        .map_err(e)?;
    let rows = stmt.query_map(params![tenant, item_id, warehouse_id], |r| Ok(json!({"item_id": r.get::<_, String>(0)?, "warehouse_id": r.get::<_, String>(1)?, "qty_kth": r.get::<_, i64>(2)?}))).map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(e)?;
    Ok(rows)
}

/// Items whose total on-hand is at or below reorder point (open PO qty not considered here).
pub fn items_below_reorder(conn: &rusqlite::Connection, tenant: &str) -> Res<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.code, i.reorder_point_kth, COALESCE((SELECT SUM(qty_kth) FROM stock_movements m WHERE m.tenant_id=i.tenant_id AND m.item_id=i.id),0) AS onhand \
         FROM items i WHERE i.tenant_id=?1 AND i.active=1 AND i.reorder_point_kth > 0 \
           AND COALESCE((SELECT SUM(qty_kth) FROM stock_movements m WHERE m.tenant_id=i.tenant_id AND m.item_id=i.id),0) <= i.reorder_point_kth \
         ORDER BY i.code").map_err(e)?;
    let rows = stmt.query_map(params![tenant], |r| Ok(json!({"item_id": r.get::<_, String>(0)?, "code": r.get::<_, String>(1)?, "reorder_point_kth": r.get::<_, i64>(2)?, "on_hand_kth": r.get::<_, i64>(3)?}))).map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(e)?;
    Ok(rows)
}

/// Reserve stock against a reference (e.g. sales order). Available = on hand minus open reservations.
pub fn reserve_stock(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    item_id: &str,
    warehouse_id: &str,
    qty_kth: i64,
    ref_type: &str,
    ref_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.reserve")?;
        if qty_kth <= 0 {
            return Err("reservation quantity must be greater than zero".to_string());
        }
        let available = on_hand(tx, &ctx.tenant, item_id, warehouse_id)?
            - reserved_qty(tx, &ctx.tenant, item_id, warehouse_id)?;
        if available < qty_kth {
            return Err(format!("insufficient available stock for item '{item_id}' (available {}.{:03}, requested {}.{:03})", available / 1000, (available % 1000).abs(), qty_kth / 1000, (qty_kth % 1000).abs()));
        }
        let id = new_id();
        tx.execute("INSERT INTO reservations(tenant_id,id,item_id,warehouse_id,qty_kth,ref_type,ref_id,status,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'open',?8)",
            params![ctx.tenant, id, item_id, warehouse_id, qty_kth, ref_type, ref_id, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "stock.reserve",
            &id,
            &json!({"item_id": item_id, "qty_kth": qty_kth, "ref_type": ref_type, "ref_id": ref_id}),
            now,
        )?;
        Ok(json!({"reservation_id": id}))
    })
}

/// Update an open reservation to released/fulfilled.
pub fn set_reservation_status(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    reservation_id: &str,
    status: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.reserve")?;
        if status != "released" && status != "fulfilled" {
            return Err("reservation status must be 'released' or 'fulfilled'".to_string());
        }
        let n = tx
            .execute(
                "UPDATE reservations SET status=?1 WHERE tenant_id=?2 AND id=?3 AND status='open'",
                params![status, ctx.tenant, reservation_id],
            )
            .map_err(e)?;
        if n == 0 {
            return Err(format!(
                "reservation '{reservation_id}' not found or not open"
            ));
        }
        audit(
            tx,
            &ctx.tenant,
            "stock.reserve.update",
            reservation_id,
            &json!({"status": status}),
            now,
        )?;
        Ok(json!({"reservation_id": reservation_id, "status": status}))
    })
}

/// Register a serial number as in stock.
pub fn register_serial(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    item_id: &str,
    serial: &str,
    warehouse_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.serials")?;
        if serial.trim().is_empty() {
            return Err("serial must not be empty".to_string());
        }
        item_row(tx, &ctx.tenant, item_id)?;
        tx.execute("INSERT INTO serial_numbers(tenant_id,item_id,serial,status,warehouse_id,created_at) VALUES (?1,?2,?3,'in_stock',?4,?5)",
            params![ctx.tenant, item_id, serial.trim(), warehouse_id, now])
            .map_err(|err| format!("serial '{serial}' cannot be registered for item '{item_id}' (duplicate?): {err}"))?;
        audit(
            tx,
            &ctx.tenant,
            "serial.register",
            serial,
            &json!({"item_id": item_id, "warehouse_id": warehouse_id}),
            now,
        )?;
        Ok(json!({"serial": serial}))
    })
}

/// Create a cycle count, snapshotting current on-hand as system quantities.
pub fn create_cycle_count(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    warehouse_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.cycle_count")?;
        let id = new_id();
        tx.execute("INSERT INTO cycle_counts(tenant_id,id,warehouse_id,status,created_by,created_at) VALUES (?1,?2,?3,'open',?4,?5)",
            params![ctx.tenant, id, warehouse_id, ctx.user, now]).map_err(e)?;
        tx.execute("INSERT INTO cycle_count_lines(tenant_id,count_id,item_id,system_qty_kth) \
                    SELECT tenant_id, ?1, item_id, SUM(qty_kth) FROM stock_movements WHERE tenant_id=?2 AND warehouse_id=?3 GROUP BY item_id",
            params![id, ctx.tenant, warehouse_id]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "cycle_count.create",
            &id,
            &json!({"warehouse_id": warehouse_id}),
            now,
        )?;
        Ok(json!({"count_id": id}))
    })
}

/// Record a counted quantity for one line of an open count.
pub fn record_count(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    count_id: &str,
    item_id: &str,
    counted_qty_kth: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.cycle_count")?;
        if counted_qty_kth < 0 {
            return Err(
                "counted quantity must not be negative (field 'counted_qty_kth')".to_string(),
            );
        }
        let status: String = tx
            .query_row(
                "SELECT status FROM cycle_counts WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, count_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("cycle count '{count_id}' not found"))?;
        if status != "open" {
            return Err(format!(
                "cycle count '{count_id}' is not open (status {status})"
            ));
        }
        let n = tx.execute("UPDATE cycle_count_lines SET counted_qty_kth=?1 WHERE tenant_id=?2 AND count_id=?3 AND item_id=?4", params![counted_qty_kth, ctx.tenant, count_id, item_id]).map_err(e)?;
        if n == 0 {
            return Err(format!(
                "item '{item_id}' is not part of cycle count '{count_id}'"
            ));
        }
        audit(
            tx,
            &ctx.tenant,
            "cycle_count.record",
            count_id,
            &json!({"item_id": item_id, "counted_qty_kth": counted_qty_kth}),
            now,
        )?;
        Ok(json!({"count_id": count_id, "item_id": item_id, "counted_qty_kth": counted_qty_kth}))
    })
}

/// Post variances of a fully-counted cycle count to stock and (if non-zero) to the ledger.
pub fn post_cycle_count(
    conn: &rusqlite::Connection,
    ledger: &dyn Ledger,
    ctx: &Ctx,
    count_id: &str,
    currency: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "inventory.cycle_count.post")?;
        let row = tx
            .query_row(
                "SELECT warehouse_id, status FROM cycle_counts WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, count_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("cycle count '{count_id}' not found"))?;
        if row.1 != "open" {
            return Err(format!(
                "cycle count '{count_id}' already posted (status {})",
                row.1
            ));
        }
        let uncounted: i64 = tx.query_row("SELECT COUNT(*) FROM cycle_count_lines WHERE tenant_id=?1 AND count_id=?2 AND counted_qty_kth IS NULL", params![ctx.tenant, count_id], |r| r.get(0)).map_err(e)?;
        if uncounted > 0 {
            return Err(format!("cycle count '{count_id}' has {uncounted} uncounted lines; count every line before posting"));
        }
        let mut stmt = tx.prepare("SELECT item_id, system_qty_kth, counted_qty_kth FROM cycle_count_lines WHERE tenant_id=?1 AND count_id=?2").map_err(e)?;
        let lines = stmt
            .query_map(params![ctx.tenant, count_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        let mut total_value: i64 = 0;
        let mut posted = 0i64;
        for (item_id, system, counted) in lines {
            let delta = counted - system;
            if delta == 0 {
                continue;
            }
            let item = item_row(tx, &ctx.tenant, &item_id)?;
            let line_value = crate::common::mmul_qty(item.wa_cost, delta.abs())?;
            let signed = if delta > 0 { line_value } else { -line_value };
            total_value = total_value
                .checked_add(signed)
                .ok_or_else(|| "variance value overflow".to_string())?;
            if delta > 0 {
                if item.method == "FIFO" {
                    fifo_push(
                        tx,
                        &ctx.tenant,
                        &item_id,
                        &row.0,
                        &None,
                        delta,
                        item.wa_cost,
                        now,
                    )?;
                }
            } else {
                fifo_consume(
                    tx,
                    &ctx.tenant,
                    &item_id,
                    &row.0,
                    -delta,
                    item.wa_cost,
                    true,
                )?;
            }
            insert_movement(
                tx,
                &ctx.tenant,
                MovementIn {
                    item_id: item_id.clone(),
                    warehouse_id: row.0.clone(),
                    bin_id: None,
                    batch_no: None,
                    mtype: "adjustment",
                    qty_kth: delta,
                    unit_cost_minor: item.wa_cost,
                    ref_type: Some("cycle_count".to_string()),
                    ref_id: Some(count_id.to_string()),
                },
                &ctx.user,
                now,
            )?;
            posted += 1;
        }
        let mut ledger_entry = None;
        if total_value != 0 {
            let (dr, cr) = if total_value > 0 {
                ("1300", "4900")
            } else {
                ("4900", "1300")
            };
            let amt = total_value.unsigned_abs() as i64;
            let entry = JournalEntry {
                date: ymd(now),
                memo: format!("cycle count {count_id} variance"),
                currency: currency.to_string(),
                lines: vec![
                    JournalLine {
                        account: dr.to_string(),
                        debit: amt,
                        credit: 0,
                    },
                    JournalLine {
                        account: cr.to_string(),
                        debit: 0,
                        credit: amt,
                    },
                ],
            };
            ledger_entry = Some(ledger.post(&ctx.tenant, &entry)?);
        }
        tx.execute(
            "UPDATE cycle_counts SET status='posted', posted_at=?1 WHERE tenant_id=?2 AND id=?3",
            params![now, ctx.tenant, count_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "cycle_count.post",
            count_id,
            &json!({"lines_posted": posted, "total_value_minor": total_value, "ledger_entry": ledger_entry}),
            now,
        )?;
        Ok(
            json!({"count_id": count_id, "lines_posted": posted, "total_value_minor": total_value, "ledger_entry": ledger_entry}),
        )
    })
}
