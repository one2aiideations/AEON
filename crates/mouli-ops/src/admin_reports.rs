//! Tenant administration (warehouses, roles, approval limits, first-run bootstrap),
//! notifications and the read-only reports.

use crate::common::*;
use crate::hr;
use crate::inventory;
use crate::procurement;
use crate::projects_service;
use crate::sales;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn create_warehouse(
    conn: &Connection,
    ctx: &Ctx,
    code: &str,
    name: &str,
    now: i64,
) -> Res<Value> {
    let code = code.trim().to_string();
    let name = name.trim().to_string();
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "admin.warehouses.create")?;
        if code.is_empty() {
            return Err("code: must not be empty".to_string());
        }
        if name.is_empty() {
            return Err("name: must not be empty".to_string());
        }
        let exists: Option<String> = tx
            .query_row(
                "SELECT code FROM warehouses WHERE tenant_id = ? AND code = ?",
                params![ctx.tenant, code],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        if let Some(c) = exists {
            return Err(format!("warehouse code '{}' already exists", c));
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO warehouses (tenant_id, id, code, name) VALUES (?, ?, ?, ?)",
            params![ctx.tenant, id, code, name],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "warehouse.create",
            &id,
            &json!({"code": code, "name": name}),
            now,
        )?;
        Ok(json!({"id": id, "code": code}))
    })
}

pub fn grant_permission(
    conn: &Connection,
    ctx: &Ctx,
    role: &str,
    permission: &str,
    now: i64,
) -> Res<Value> {
    let role = role.trim().to_string();
    let permission = permission.trim().to_string();
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "admin.roles.manage")?;
        if role.is_empty() {
            return Err("role: must not be empty".to_string());
        }
        if permission.is_empty() {
            return Err("permission: must not be empty".to_string());
        }
        tx.execute(
            "INSERT OR IGNORE INTO role_permissions (tenant_id, role, permission) VALUES (?, ?, ?)",
            params![ctx.tenant, role, permission],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "permission.grant",
            &role,
            &json!({"role": role, "permission": permission}),
            now,
        )?;
        Ok(json!({"role": role, "permission": permission}))
    })
}

pub fn revoke_permission(
    conn: &Connection,
    ctx: &Ctx,
    role: &str,
    permission: &str,
    now: i64,
) -> Res<Value> {
    let role = role.trim().to_string();
    let permission = permission.trim().to_string();
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "admin.roles.manage")?;
        if role.is_empty() {
            return Err("role: must not be empty".to_string());
        }
        if permission.is_empty() {
            return Err("permission: must not be empty".to_string());
        }
        let n = tx
            .execute(
                "DELETE FROM role_permissions WHERE tenant_id = ? AND role = ? AND permission = ?",
                params![ctx.tenant, role, permission],
            )
            .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "permission.revoke",
            &role,
            &json!({"role": role, "permission": permission, "removed": n > 0}),
            now,
        )?;
        Ok(json!({"removed": n > 0}))
    })
}

pub fn set_approval_limit(
    conn: &Connection,
    ctx: &Ctx,
    role: &str,
    limit_minor: i64,
    now: i64,
) -> Res<Value> {
    let role = role.trim().to_string();
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "admin.roles.manage")?;
        if role.is_empty() {
            return Err("role: must not be empty".to_string());
        }
        if !(limit_minor == -1 || limit_minor >= 0) {
            return Err("limit_minor: must be -1 (unlimited) or >= 0".to_string());
        }
        tx.execute(
            "INSERT INTO approval_limits (tenant_id, role, limit_minor) VALUES (?, ?, ?) \
             ON CONFLICT(tenant_id, role) DO UPDATE SET limit_minor = excluded.limit_minor",
            params![ctx.tenant, role, limit_minor],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "approval_limit.set",
            &role,
            &json!({"role": role, "limit_minor": limit_minor}),
            now,
        )?;
        Ok(json!({"role": role, "limit_minor": limit_minor}))
    })
}

pub fn bootstrap_tenant(conn: &Connection, tenant: &str, admin_user: &str, now: i64) -> Res<Value> {
    let tenant = tenant.trim().to_string();
    let admin_user = admin_user.trim().to_string();
    if tenant.is_empty() {
        return Err("tenant: must not be empty".to_string());
    }
    if admin_user.is_empty() {
        return Err("admin_user: must not be empty".to_string());
    }
    in_tx(conn, |tx| {
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM role_permissions WHERE tenant_id = ?",
                params![tenant],
                |r| r.get(0),
            )
            .map_err(e)?;
        if count > 0 {
            return Err(format!(
                "tenant '{}' already bootstrapped: role_permissions is not empty",
                tenant
            ));
        }
        tx.execute(
            "INSERT INTO role_permissions (tenant_id, role, permission) VALUES (?, ?, ?)",
            params![tenant, "admin", "*"],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO approval_limits (tenant_id, role, limit_minor) VALUES (?, ?, ?) \
             ON CONFLICT(tenant_id, role) DO UPDATE SET limit_minor = excluded.limit_minor",
            params![tenant, "admin", -1i64],
        )
        .map_err(e)?;
        audit(
            tx,
            &tenant,
            "tenant.bootstrap",
            &tenant,
            &json!({"user": admin_user, "role": "admin"}),
            now,
        )?;
        Ok(json!({"tenant": tenant, "role": "admin", "user": admin_user}))
    })
}

pub fn mark_notification_read(
    conn: &Connection,
    ctx: &Ctx,
    notification_id: &str,
    now: i64,
) -> Res<Value> {
    let notification_id = notification_id.trim().to_string();
    if notification_id.is_empty() {
        return Err("notification_id: must not be empty".to_string());
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "notifications.read")?;
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT user_name, read_flag FROM notifications WHERE tenant_id = ? AND id = ?",
                params![ctx.tenant, notification_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?;
        let (owner, _) =
            row.ok_or_else(|| format!("notification '{}' not found", notification_id))?;
        if owner != ctx.user {
            return Err(format!(
                "notification '{}' cannot be marked read by user '{}': belongs to a different user",
                notification_id, ctx.user
            ));
        }
        tx.execute(
            "UPDATE notifications SET read_flag = 1 WHERE tenant_id = ? AND id = ?",
            params![ctx.tenant, notification_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "notification.read",
            &notification_id,
            &json!({"notification_id": notification_id}),
            now,
        )?;
        Ok(json!({"id": notification_id, "read": true}))
    })
}

pub fn list_notifications(
    conn: &Connection,
    tenant: &str,
    user: &str,
    unread_only: bool,
    limit: i64,
) -> Res<Vec<Value>> {
    let limit = limit.clamp(1, 500);
    let mut stmt = if unread_only {
        conn.prepare(
            "SELECT id, kind, payload, read_flag, created_at FROM notifications \
             WHERE tenant_id = ? AND user_name = ? AND read_flag = 0 \
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
    } else {
        conn.prepare(
            "SELECT id, kind, payload, read_flag, created_at FROM notifications \
             WHERE tenant_id = ? AND user_name = ? \
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
    }
    .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant, user, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(e)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, kind, payload_str, read_flag, created_at) = row.map_err(e)?;
        let payload: Value = serde_json::from_str(&payload_str)
            .unwrap_or_else(|_| Value::String(payload_str.clone()));
        out.push(json!({
            "id": id,
            "kind": kind,
            "payload": payload,
            "read": read_flag != 0,
            "created_at": created_at,
        }));
    }
    Ok(out)
}

pub fn list_warehouses(conn: &Connection, tenant: &str) -> Res<Vec<Value>> {
    let mut stmt = conn
        .prepare("SELECT id, code, name FROM warehouses WHERE tenant_id = ? ORDER BY code ASC")
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(e)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, code, name) = row.map_err(e)?;
        out.push(json!({"id": id, "code": code, "name": name}));
    }
    Ok(out)
}

pub fn sales_by_period(conn: &Connection, ctx: &Ctx, from_ts: i64, to_ts: i64) -> Res<Value> {
    require_perm(conn, ctx, "reports.sales_by_period")?;
    if from_ts > to_ts {
        return Err("from_ts: must be <= to_ts".to_string());
    }
    let invoices = sales::invoices_between(conn, &ctx.tenant, from_ts, to_ts)?;
    let mut by_cur: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    let mut by_cust: BTreeMap<String, i64> = BTreeMap::new();
    for inv in &invoices {
        let currency = jstr(inv, "currency")?;
        let total = jint(inv, "total_minor")?;
        let tax = jint(inv, "tax_minor")?;
        let net = msub(total, tax)?;
        let entry = by_cur.entry(currency).or_insert((0, 0, 0));
        entry.0 = madd(entry.0, net)?;
        entry.1 = madd(entry.1, tax)?;
        entry.2 = madd(entry.2, total)?;
        let customer_id = jstr(inv, "customer_id")?;
        let acc = by_cust.entry(customer_id).or_insert(0);
        *acc = madd(*acc, total)?;
    }
    let by_currency: Vec<Value> = by_cur
        .into_iter()
        .map(|(currency, (net_minor, tax_minor, total_minor))| {
            json!({
                "currency": currency,
                "net_minor": net_minor,
                "tax_minor": tax_minor,
                "total_minor": total_minor,
            })
        })
        .collect();
    let mut top: Vec<(String, i64)> = by_cust.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(5);
    let top_customers: Vec<Value> = top
        .into_iter()
        .map(|(customer_id, total_minor)| json!({"customer_id": customer_id, "total_minor": total_minor}))
        .collect();
    Ok(json!({
        "from_ts": from_ts,
        "to_ts": to_ts,
        "invoice_count": invoices.len(),
        "by_currency": by_currency,
        "top_customers": top_customers,
    }))
}

pub fn open_orders(conn: &Connection, ctx: &Ctx) -> Res<Value> {
    require_perm(conn, ctx, "reports.open_orders")?;
    let orders = sales::open_orders(conn, &ctx.tenant)?;
    let mut value_minor: i64 = 0;
    for o in &orders {
        value_minor = madd(value_minor, jint(o, "total_minor")?)?;
    }
    let count = orders.len();
    Ok(json!({"orders": orders, "count": count, "value_minor": value_minor}))
}

pub fn stock(conn: &Connection, ctx: &Ctx, warehouse_id: Option<&str>) -> Res<Value> {
    require_perm(conn, ctx, "reports.stock")?;
    let rows = inventory::stock_on_hand_rows(conn, &ctx.tenant, None, warehouse_id)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let item_id = jstr(row, "item_id")?;
        let qty_kth = jint(row, "qty_kth")?;
        let wh = jstr(row, "warehouse_id")?;
        let code: Option<String> = conn
            .query_row(
                "SELECT code FROM items WHERE tenant_id = ? AND id = ?",
                params![ctx.tenant, item_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        out.push(json!({
            "item_id": item_id,
            "code": code.unwrap_or_default(),
            "warehouse_id": wh,
            "qty_kth": qty_kth,
        }));
    }
    let total_value_minor = inventory::stock_value_minor(conn, &ctx.tenant, warehouse_id)?;
    Ok(json!({"rows": out, "total_value_minor": total_value_minor}))
}

pub fn reorder(conn: &Connection, ctx: &Ctx) -> Res<Value> {
    require_perm(conn, ctx, "reports.reorder")?;
    let items = inventory::items_below_reorder(conn, &ctx.tenant)?;
    let count = items.len();
    Ok(json!({"items": items, "count": count}))
}

pub fn purchase_ageing(conn: &Connection, ctx: &Ctx, as_of: i64) -> Res<Value> {
    require_perm(conn, ctx, "reports.purchase_ageing")?;
    procurement::purchase_ageing(conn, &ctx.tenant, as_of)
}

pub fn payroll(conn: &Connection, ctx: &Ctx, period: &str) -> Res<Value> {
    require_perm(conn, ctx, "reports.payroll")?;
    hr::payroll_summary(conn, &ctx.tenant, period)
}

pub fn sla_compliance(conn: &Connection, ctx: &Ctx) -> Res<Value> {
    require_perm(conn, ctx, "reports.sla_compliance")?;
    projects_service::sla_compliance(conn, &ctx.tenant)
}

pub fn project_status(conn: &Connection, ctx: &Ctx, project_id: &str) -> Res<Value> {
    require_perm(conn, ctx, "reports.project_status")?;
    projects_service::project_status(conn, &ctx.tenant, project_id)
}

pub fn audit_integrity(conn: &Connection, ctx: &Ctx) -> Res<Value> {
    require_perm(conn, ctx, "reports.audit_integrity")?;
    let verified_rows = verify_audit(conn, &ctx.tenant)?;
    Ok(json!({"verified_rows": verified_rows}))
}

pub fn run_report(conn: &Connection, ctx: &Ctx, name: &str, args: &Value) -> Res<Value> {
    match name {
        "sales_by_period" => sales_by_period(conn, ctx, jint(args, "from_ts")?, jint(args, "to_ts")?),
        "open_orders" => open_orders(conn, ctx),
        "stock" => stock(conn, ctx, jopt_str(args, "warehouse_id").as_deref()),
        "reorder" => reorder(conn, ctx),
        "purchase_ageing" => purchase_ageing(conn, ctx, jint(args, "as_of")?),
        "payroll" => payroll(conn, ctx, &jstr(args, "period")?),
        "sla_compliance" => sla_compliance(conn, ctx),
        "project_status" => project_status(conn, ctx, &jstr(args, "project_id")?),
        "audit_integrity" => audit_integrity(conn, ctx),
        _ => Err(format!(
            "unknown report '{}': valid reports are sales_by_period, open_orders, stock, reorder, purchase_ageing, payroll, sla_compliance, project_status, audit_integrity",
            name
        )),
    }
}
