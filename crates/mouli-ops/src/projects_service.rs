//! Projects, timesheets and billing, milestones, and service tickets with working-hours SLA clocks.

use crate::common::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

fn today(now: i64) -> String {
    ymd(now).chars().take(10).collect()
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> Res<i64> {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Ok(31),
        4 | 6 | 9 | 11 => Ok(30),
        2 => Ok(if is_leap(y) { 29 } else { 28 }),
        _ => Err(format!("field 'month' must be between 1 and 12, got {}", m)),
    }
}

fn parse_date(s: &str) -> Res<(i64, i64, i64)> {
    let mut parts = s.split('-');
    let y: i64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("field 'date' must be in YYYY-MM-DD format, got '{}'", s))?;
    let m: i64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("field 'date' must be in YYYY-MM-DD format, got '{}'", s))?;
    let d: i64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("field 'date' must be in YYYY-MM-DD format, got '{}'", s))?;
    if parts.next().is_some() {
        return Err(format!(
            "field 'date' must be in YYYY-MM-DD format, got '{}'",
            s
        ));
    }
    let dim = days_in_month(y, m)?;
    if d < 1 || d > dim {
        return Err(format!(
            "field 'date' day {} is out of range for {:04}-{:02}",
            d, y, m
        ));
    }
    Ok((y, m, d))
}

/// Creates an active project. [projects.project.create]
pub fn create_project(
    conn: &Connection,
    ctx: &Ctx,
    code: &str,
    name: &str,
    customer_id: Option<&str>,
    billable: bool,
    rate_minor_per_hour: i64,
    budget_hours_kth: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.project.create")?;
        if code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        if name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if rate_minor_per_hour < 0 {
            return Err(
                "field 'rate_minor_per_hour' must be greater than or equal to zero".to_string(),
            );
        }
        if budget_hours_kth < 0 {
            return Err(
                "field 'budget_hours_kth' must be greater than or equal to zero".to_string(),
            );
        }
        if let Some(cid) = customer_id {
            let ok: bool = tx
                .query_row(
                    "SELECT COUNT(*) FROM customers WHERE tenant_id = ?1 AND id = ?2",
                    params![ctx.tenant, cid],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(e)?
                > 0;
            if !ok {
                return Err(format!(
                    "field 'customer_id' references unknown customer '{}'",
                    cid
                ));
            }
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO projects (tenant_id, id, code, name, customer_id, billable, rate_minor_per_hour, budget_hours_kth, status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'active')",
            params![ctx.tenant, id, code, name, customer_id, if billable { 1 } else { 0 }, rate_minor_per_hour, budget_hours_kth],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "project.created",
            &id,
            &json!({"code": code}),
            now,
        )?;
        Ok(json!({"id": id, "code": code, "status": "active"}))
    })
}

/// Creates an open task under a project. [projects.task.create]
pub fn create_task(
    conn: &Connection,
    ctx: &Ctx,
    project_id: &str,
    title: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.task.create")?;
        if title.trim().is_empty() {
            return Err("field 'title' must not be empty".to_string());
        }
        let ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, project_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !ok {
            return Err(format!("project '{}' not found", project_id));
        }
        let id = new_id();
        tx.execute("INSERT INTO tasks (tenant_id, id, project_id, title, status) VALUES (?1,?2,?3,?4,'open')", params![ctx.tenant, id, project_id, title]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "task.created",
            &id,
            &json!({"project_id": project_id}),
            now,
        )?;
        Ok(json!({"id": id, "project_id": project_id, "status": "open"}))
    })
}

/// Logs time against a project (hourly rate copied from the project). [projects.time.log]
pub fn log_time(
    conn: &Connection,
    ctx: &Ctx,
    project_id: &str,
    task_id: Option<&str>,
    employee_id: &str,
    date: &str,
    hours_kth: i64,
    billable: bool,
    now: i64,
) -> Res<Value> {
    parse_date(date)?;
    if hours_kth <= 0 || hours_kth > 24_000 {
        return Err(
            "field 'hours_kth' must be between 1 and 24000 thousandths of an hour".to_string(),
        );
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.time.log")?;
        let (status, rate): (String, i64) = tx
            .query_row(
                "SELECT status, rate_minor_per_hour FROM projects WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("project '{}' not found", project_id))?;
        if status == "closed" {
            return Err(format!("project '{}' is closed", project_id));
        }
        if let Some(tid) = task_id {
            let ok: bool = tx
                .query_row("SELECT COUNT(*) FROM tasks WHERE tenant_id = ?1 AND id = ?2 AND project_id = ?3", params![ctx.tenant, tid, project_id], |r| r.get::<_, i64>(0))
                .map_err(e)? > 0;
            if !ok {
                return Err(format!(
                    "field 'task_id' references unknown task '{}' in project '{}'",
                    tid, project_id
                ));
            }
        }
        let day_total: i64 = tx
            .query_row("SELECT COALESCE(SUM(hours_kth), 0) FROM timesheets WHERE tenant_id = ?1 AND employee_id = ?2 AND date = ?3", params![ctx.tenant, employee_id, date], |r| r.get(0))
            .map_err(e)?;
        let new_total = qadd(day_total, hours_kth)?;
        if new_total > 24_000 {
            return Err(format!("field 'hours_kth' exceeds the daily limit: employee '{}' has already logged {} thousandths on {}, limit is 24000", employee_id, day_total, date));
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO timesheets (tenant_id, id, project_id, task_id, employee_id, date, hours_kth, billable, rate_minor_per_hour, approved, invoiced) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,0)",
            params![ctx.tenant, id, project_id, task_id, employee_id, date, hours_kth, if billable { 1 } else { 0 }, rate],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "time.logged",
            &id,
            &json!({"project_id": project_id, "hours_kth": hours_kth, "date": date}),
            now,
        )?;
        Ok(
            json!({"id": id, "project_id": project_id, "hours_kth": hours_kth, "rate_minor_per_hour": rate}),
        )
    })
}

/// Approves a timesheet. [projects.time.approve]
pub fn approve_timesheet(conn: &Connection, ctx: &Ctx, timesheet_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.time.approve")?;
        let n = tx
            .execute(
                "UPDATE timesheets SET approved = 1 WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, timesheet_id],
            )
            .map_err(e)?;
        if n == 0 {
            return Err(format!("timesheet '{}' not found", timesheet_id));
        }
        audit(
            tx,
            &ctx.tenant,
            "timesheet.approved",
            timesheet_id,
            &json!({}),
            now,
        )?;
        Ok(json!({"id": timesheet_id, "approved": true}))
    })
}

/// Invoices approved, billable, not-yet-invoiced timesheets of a project. [projects.invoice]
pub fn invoice_timesheets(
    conn: &Connection,
    ctx: &Ctx,
    project_id: &str,
    ledger: &dyn Ledger,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.invoice")?;
        let customer_id: Option<String> = tx
            .query_row(
                "SELECT customer_id FROM projects WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, project_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("project '{}' not found", project_id))?;
        let customer_id = customer_id.ok_or_else(|| {
            format!(
                "field 'customer_id' is required on project '{}' to invoice",
                project_id
            )
        })?;
        let currency: String = tx
            .query_row(
                "SELECT currency FROM customers WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, customer_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("customer '{}' not found", customer_id))?;
        let mut stmt = tx
            .prepare("SELECT id, date, hours_kth, rate_minor_per_hour FROM timesheets WHERE tenant_id = ?1 AND project_id = ?2 AND approved = 1 AND billable = 1 AND invoiced = 0 ORDER BY date ASC, id ASC")
            .map_err(e)?;
        let entries = stmt
            .query_map(params![ctx.tenant, project_id], |r| {
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
        if entries.is_empty() {
            return Err(format!(
                "no approved billable uninvoiced timesheets to invoice for project '{}'",
                project_id
            ));
        }
        let mut total: i64 = 0;
        for (_, _, hours, rate) in &entries {
            total = madd(total, mmul_qty(*rate, *hours)?)?;
        }
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "INV", now)?;
        let invoice_id = new_id();
        tx.execute(
            "INSERT INTO invoices (tenant_id, id, number, customer_id, order_id, currency, total_minor, tax_minor, status, due_date, created_by, created_at) VALUES (?1,?2,?3,?4,NULL,?5,?6,0,'posted',NULL,?7,?8)",
            params![ctx.tenant, invoice_id, number, customer_id, currency, total, ctx.user, now],
        )
        .map_err(e)?;
        for (line_no, (ts_id, date, hours, rate)) in (1_i64..).zip(entries.iter()) {
            let line_total = mmul_qty(*rate, *hours)?;
            tx.execute(
                "INSERT INTO invoice_lines (tenant_id, invoice_id, line_no, item_id, description, qty_kth, unit_price_minor, total_minor) VALUES (?1,?2,?3,NULL,?4,?5,?6,?7)",
                params![ctx.tenant, invoice_id, line_no, format!("Timesheet {}", date), hours, rate, line_total],
            )
            .map_err(e)?;
            tx.execute(
                "UPDATE timesheets SET invoiced = 1 WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ts_id],
            )
            .map_err(e)?;
        }
        ledger.post(
            &ctx.tenant,
            &JournalEntry {
                date: today(now),
                memo: format!("Invoice {} for project {}", number, project_id),
                currency,
                lines: vec![
                    JournalLine {
                        account: "1100".to_string(),
                        debit: total,
                        credit: 0,
                    },
                    JournalLine {
                        account: "4100".to_string(),
                        debit: 0,
                        credit: total,
                    },
                ],
            },
        )?;
        audit(
            tx,
            &ctx.tenant,
            "project.invoiced",
            &invoice_id,
            &json!({"number": number, "project_id": project_id, "total_minor": total}),
            now,
        )?;
        Ok(
            json!({"id": invoice_id, "number": number, "status": "posted", "total_minor": total, "lines": entries.len()}),
        )
    })
}

/// Adds a milestone (status `pending`) to a project. [projects.milestone.add]
pub fn add_milestone(
    conn: &Connection,
    ctx: &Ctx,
    project_id: &str,
    name: &str,
    amount_minor: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.milestone.add")?;
        if name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if amount_minor < 0 {
            return Err("field 'amount_minor' must be greater than or equal to zero".to_string());
        }
        let ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, project_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !ok {
            return Err(format!("project '{}' not found", project_id));
        }
        let id = new_id();
        tx.execute("INSERT INTO milestones (tenant_id, id, project_id, name, amount_minor, status) VALUES (?1,?2,?3,?4,?5,'pending')", params![ctx.tenant, id, project_id, name, amount_minor]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "milestone.added",
            &id,
            &json!({"project_id": project_id, "amount_minor": amount_minor}),
            now,
        )?;
        Ok(json!({"id": id, "project_id": project_id, "status": "pending"}))
    })
}

/// Marks a pending milestone as achieved. [projects.milestone.achieve]
pub fn achieve_milestone(conn: &Connection, ctx: &Ctx, milestone_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.milestone.achieve")?;
        let status: String = tx
            .query_row(
                "SELECT status FROM milestones WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, milestone_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("milestone '{}' not found", milestone_id))?;
        if status != "pending" {
            return Err(format!(
                "field 'status' must be 'pending' to achieve, got '{}'",
                status
            ));
        }
        tx.execute(
            "UPDATE milestones SET status = 'achieved' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, milestone_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "milestone.achieved",
            milestone_id,
            &json!({}),
            now,
        )?;
        Ok(json!({"id": milestone_id, "status": "achieved"}))
    })
}

/// Invoices an achieved milestone and sets status `invoiced` with `invoice_id`. [projects.milestone.invoice]
pub fn invoice_milestone(
    conn: &Connection,
    ctx: &Ctx,
    milestone_id: &str,
    ledger: &dyn Ledger,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "projects.milestone.invoice")?;
        let (project_id, name, amount, status): (String, String, i64, String) = tx
            .query_row("SELECT project_id, name, amount_minor, status FROM milestones WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, milestone_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("milestone '{}' not found", milestone_id))?;
        if status != "achieved" {
            return Err(format!(
                "field 'status' must be 'achieved' to invoice, got '{}'",
                status
            ));
        }
        let customer_id: Option<String> = tx
            .query_row(
                "SELECT customer_id FROM projects WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, project_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("project '{}' not found", project_id))?;
        let customer_id = customer_id.ok_or_else(|| {
            format!(
                "field 'customer_id' is required on project '{}' to invoice",
                project_id
            )
        })?;
        let currency: String = tx
            .query_row(
                "SELECT currency FROM customers WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, customer_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("customer '{}' not found", customer_id))?;
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "INV", now)?;
        let invoice_id = new_id();
        tx.execute(
            "INSERT INTO invoices (tenant_id, id, number, customer_id, order_id, currency, total_minor, tax_minor, status, due_date, created_by, created_at) VALUES (?1,?2,?3,?4,NULL,?5,?6,0,'posted',NULL,?7,?8)",
            params![ctx.tenant, invoice_id, number, customer_id, currency, amount, ctx.user, now],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO invoice_lines (tenant_id, invoice_id, line_no, item_id, description, qty_kth, unit_price_minor, total_minor) VALUES (?1,?2,1,NULL,?3,1000,?4,?4)",
            params![ctx.tenant, invoice_id, name, amount],
        )
        .map_err(e)?;
        tx.execute("UPDATE milestones SET status = 'invoiced', invoice_id = ?3 WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, milestone_id, invoice_id]).map_err(e)?;
        ledger.post(
            &ctx.tenant,
            &JournalEntry {
                date: today(now),
                memo: format!("Milestone {} invoiced", milestone_id),
                currency,
                lines: vec![
                    JournalLine {
                        account: "1100".to_string(),
                        debit: amount,
                        credit: 0,
                    },
                    JournalLine {
                        account: "4100".to_string(),
                        debit: 0,
                        credit: amount,
                    },
                ],
            },
        )?;
        audit(
            tx,
            &ctx.tenant,
            "milestone.invoiced",
            &invoice_id,
            &json!({"number": number, "milestone_id": milestone_id}),
            now,
        )?;
        Ok(
            json!({"id": invoice_id, "number": number, "status": "posted", "total_minor": amount, "milestone_id": milestone_id}),
        )
    })
}

/// Read helper: hours and billing status of a project.
pub fn project_status(conn: &Connection, tenant: &str, project_id: &str) -> Res<Value> {
    let budget: i64 = conn
        .query_row(
            "SELECT budget_hours_kth FROM projects WHERE tenant_id = ?1 AND id = ?2",
            params![tenant, project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("project '{}' not found", project_id))?;
    let hours_logged: i64 = conn
        .query_row("SELECT COALESCE(SUM(hours_kth), 0) FROM timesheets WHERE tenant_id = ?1 AND project_id = ?2", params![tenant, project_id], |r| r.get(0))
        .map_err(e)?;
    let mut stmt = conn
        .prepare("SELECT hours_kth, rate_minor_per_hour, approved, billable, invoiced FROM timesheets WHERE tenant_id = ?1 AND project_id = ?2")
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant, project_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut billed: i64 = 0;
    let mut unbilled: i64 = 0;
    for (hours, rate, approved, billable, invoiced) in rows {
        let value = mmul_qty(rate, hours)?;
        if invoiced == 1 {
            billed = madd(billed, value)?;
        } else if approved == 1 && billable == 1 {
            unbilled = madd(unbilled, value)?;
        }
    }
    let billed_ms: i64 = conn
        .query_row("SELECT COALESCE(SUM(amount_minor), 0) FROM milestones WHERE tenant_id = ?1 AND project_id = ?2 AND status = 'invoiced'", params![tenant, project_id], |r| r.get(0))
        .map_err(e)?;
    let unbilled_ms: i64 = conn
        .query_row("SELECT COALESCE(SUM(amount_minor), 0) FROM milestones WHERE tenant_id = ?1 AND project_id = ?2 AND status = 'achieved'", params![tenant, project_id], |r| r.get(0))
        .map_err(e)?;
    billed = madd(billed, billed_ms)?;
    unbilled = madd(unbilled, unbilled_ms)?;
    Ok(
        json!({"hours_logged_kth": hours_logged, "hours_budget_kth": budget, "billed_minor": billed, "unbilled_minor": unbilled}),
    )
}

/// Creates an SLA policy. [service.sla.create]
pub fn create_sla_policy(
    conn: &Connection,
    ctx: &Ctx,
    name: &str,
    response_minutes: i64,
    resolve_minutes: i64,
    escalation_minutes: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.sla.create")?;
        if name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if response_minutes < 0 || resolve_minutes < 0 || escalation_minutes < 0 {
            return Err("field 'response_minutes', 'resolve_minutes' and 'escalation_minutes' must be greater than or equal to zero".to_string());
        }
        let id = new_id();
        tx.execute("INSERT INTO sla_policies (tenant_id, id, name, response_minutes, resolve_minutes, escalation_minutes) VALUES (?1,?2,?3,?4,?5,?6)", params![ctx.tenant, id, name, response_minutes, resolve_minutes, escalation_minutes]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "sla.policy_created",
            &id,
            &json!({"name": name}),
            now,
        )?;
        Ok(json!({"id": id, "name": name}))
    })
}

/// Overrides whether a date is a working day (default Mon-Fri). [service.calendar.set]
pub fn set_calendar_day(
    conn: &Connection,
    ctx: &Ctx,
    date: &str,
    working: bool,
    now: i64,
) -> Res<Value> {
    parse_date(date)?;
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.calendar.set")?;
        tx.execute(
            "INSERT INTO calendar_days (tenant_id, date, working) VALUES (?1,?2,?3) ON CONFLICT(tenant_id, date) DO UPDATE SET working = excluded.working",
            params![ctx.tenant, date, if working { 1 } else { 0 }],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "calendar.day_set",
            date,
            &json!({"working": working}),
            now,
        )?;
        Ok(json!({"date": date, "working": working}))
    })
}

/// Counts business minutes (09:00-17:00 UTC) of working days between two timestamps; `from_ts >= to_ts` gives 0.
pub fn business_minutes_between(
    conn: &Connection,
    tenant: &str,
    from_ts: i64,
    to_ts: i64,
) -> Res<i64> {
    if from_ts >= to_ts {
        return Ok(0);
    }
    let mut overrides: HashMap<String, bool> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT date, working FROM calendar_days WHERE tenant_id = ?1")
            .map_err(e)?;
        let rows = stmt
            .query_map(params![tenant], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        for (date, working) in rows {
            overrides.insert(date, working != 0);
        }
    }
    let mut total_minutes: i64 = 0;
    let start_day = from_ts.div_euclid(86_400);
    let end_day = to_ts.div_euclid(86_400);
    let mut d = start_day;
    while d <= end_day {
        let weekday = (d + 4).rem_euclid(7); // 0=Sunday .. 6=Saturday
        let day_ts = d.checked_mul(86_400).ok_or("day timestamp overflow")?;
        let date = ymd(day_ts).chars().take(10).collect::<String>();
        let working = match overrides.get(&date) {
            Some(w) => *w,
            None => (1..=5).contains(&weekday),
        };
        if working {
            let day_start = day_ts.checked_add(9 * 3_600).ok_or("day start overflow")?;
            let day_end = day_ts.checked_add(17 * 3_600).ok_or("day end overflow")?;
            let s = from_ts.max(day_start);
            let en = to_ts.min(day_end);
            if en > s {
                total_minutes = total_minutes
                    .checked_add((en - s) / 60)
                    .ok_or("business minutes overflow")?;
            }
        }
        d += 1;
    }
    Ok(total_minutes)
}

/// Creates a ticket with the SLA clock started at `now`. [service.ticket.create]
pub fn create_ticket(
    conn: &Connection,
    ctx: &Ctx,
    subject: &str,
    customer_id: Option<&str>,
    priority: &str,
    sla_policy_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.ticket.create")?;
        if subject.trim().is_empty() {
            return Err("field 'subject' must not be empty".to_string());
        }
        match priority {
            "low" | "normal" | "high" | "urgent" => {}
            other => {
                return Err(format!(
                    "field 'priority' must be 'low', 'normal', 'high' or 'urgent', got '{}'",
                    other
                ))
            }
        }
        let ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM sla_policies WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, sla_policy_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !ok {
            return Err(format!(
                "field 'sla_policy_id' references unknown SLA policy '{}'",
                sla_policy_id
            ));
        }
        if let Some(cid) = customer_id {
            let ok2: bool = tx
                .query_row(
                    "SELECT COUNT(*) FROM customers WHERE tenant_id = ?1 AND id = ?2",
                    params![ctx.tenant, cid],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(e)?
                > 0;
            if !ok2 {
                return Err(format!(
                    "field 'customer_id' references unknown customer '{}'",
                    cid
                ));
            }
        }
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "TK", now)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO tickets (tenant_id, id, number, subject, customer_id, priority, status, sla_policy_id, assignee, created_at, sla_accum_minutes, clock_started_at, paused_flag, breach_notified, escalated) VALUES (?1,?2,?3,?4,?5,?6,'open',?7,NULL,?8,0,?8,0,0,0)",
            params![ctx.tenant, id, number, subject, customer_id, priority, sla_policy_id, now],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "ticket.created",
            &id,
            &json!({"number": number}),
            now,
        )?;
        Ok(
            json!({"id": id, "number": number, "status": "open", "sla_accum_minutes": 0, "clock_started_at": now}),
        )
    })
}

/// Pauses an open ticket: accumulates business minutes into `sla_accum_minutes` and stops the clock. [service.ticket.update]
pub fn pause_ticket(conn: &Connection, ctx: &Ctx, ticket_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.ticket.update")?;
        let (status, accum, clock_started): (String, i64, i64) = tx
            .query_row("SELECT status, sla_accum_minutes, clock_started_at FROM tickets WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, ticket_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("ticket '{}' not found", ticket_id))?;
        if status != "open" {
            return Err(format!(
                "field 'status' must be 'open' to pause, got '{}'",
                status
            ));
        }
        let business = business_minutes_between(tx, &ctx.tenant, clock_started, now)?;
        let new_accum = qadd(accum, business)?;
        tx.execute("UPDATE tickets SET sla_accum_minutes = ?3, paused_flag = 1, status = 'paused' WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, ticket_id, new_accum]).map_err(e)?;
        tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'pause',?4,?5,?6)", params![ctx.tenant, new_id(), ticket_id, format!("+{} business minutes", business), ctx.user, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "ticket.paused",
            ticket_id,
            &json!({"sla_accum_minutes": new_accum}),
            now,
        )?;
        Ok(json!({"id": ticket_id, "status": "paused", "sla_accum_minutes": new_accum}))
    })
}

/// Resumes a paused ticket: restarts the SLA clock. [service.ticket.update]
pub fn resume_ticket(conn: &Connection, ctx: &Ctx, ticket_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.ticket.update")?;
        let status: String = tx
            .query_row(
                "SELECT status FROM tickets WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ticket_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("ticket '{}' not found", ticket_id))?;
        if status != "paused" {
            return Err(format!(
                "field 'status' must be 'paused' to resume, got '{}'",
                status
            ));
        }
        tx.execute("UPDATE tickets SET clock_started_at = ?3, paused_flag = 0, status = 'open' WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, ticket_id, now]).map_err(e)?;
        tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'resume',NULL,?4,?5)", params![ctx.tenant, new_id(), ticket_id, ctx.user, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "ticket.resumed",
            ticket_id,
            &json!({}),
            now,
        )?;
        Ok(json!({"id": ticket_id, "status": "open", "clock_started_at": now}))
    })
}

/// Checks the SLA for a ticket; escalates once past the escalation threshold and notifies once on breach. [service.sla.check]
pub fn check_sla(conn: &Connection, ctx: &Ctx, ticket_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.sla.check")?;
        let (status, accum, clock_started, paused, escalated, breach_notified, assignee, policy_id): (String, i64, i64, i64, i64, i64, Option<String>, String) = tx
            .query_row(
                "SELECT status, sla_accum_minutes, clock_started_at, paused_flag, escalated, breach_notified, assignee, sla_policy_id FROM tickets WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ticket_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("ticket '{}' not found", ticket_id))?;
        let (resolve_minutes, escalation_minutes): (i64, i64) = tx
            .query_row("SELECT resolve_minutes, escalation_minutes FROM sla_policies WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, policy_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("sla policy '{}' not found", policy_id))?;
        let running = paused == 0 && status != "resolved";
        let business = if running {
            business_minutes_between(tx, &ctx.tenant, clock_started, now)?
        } else {
            0
        };
        let elapsed = qadd(accum, business)?;
        let mut esc = escalated != 0;
        if elapsed > escalation_minutes && !esc {
            tx.execute(
                "UPDATE tickets SET escalated = 1 WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ticket_id],
            )
            .map_err(e)?;
            tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'escalate',?4,?5,?6)", params![ctx.tenant, new_id(), ticket_id, format!("elapsed {} minutes exceeds escalation threshold {}", elapsed, escalation_minutes), ctx.user, now]).map_err(e)?;
            let target = assignee
                .clone()
                .unwrap_or_else(|| "service-manager".to_string());
            notify(
                tx,
                &ctx.tenant,
                &target,
                "ticket.escalated",
                &json!({"ticket_id": ticket_id, "elapsed_minutes": elapsed}),
                now,
            )?;
            esc = true;
        }
        let mut already_breached = breach_notified != 0;
        let mut breached = false;
        if elapsed > resolve_minutes && !already_breached {
            tx.execute(
                "UPDATE tickets SET breach_notified = 1 WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ticket_id],
            )
            .map_err(e)?;
            tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'breach',?4,?5,?6)", params![ctx.tenant, new_id(), ticket_id, format!("elapsed {} minutes exceeds resolve threshold {}", elapsed, resolve_minutes), ctx.user, now]).map_err(e)?;
            notify(
                tx,
                &ctx.tenant,
                "service-manager",
                "sla.breach",
                &json!({"ticket_id": ticket_id, "elapsed_minutes": elapsed}),
                now,
            )?;
            breached = true;
            already_breached = true;
        }
        audit(
            tx,
            &ctx.tenant,
            "ticket.sla_checked",
            ticket_id,
            &json!({"elapsed_minutes": elapsed, "breached": breached, "escalated": esc}),
            now,
        )?;
        Ok(
            json!({"elapsed_minutes": elapsed, "breached": breached, "escalated": esc, "already_breached": already_breached}),
        )
    })
}

/// Resolves a ticket: stops the SLA clock and stamps `resolved_at`. [service.ticket.update]
pub fn resolve_ticket(conn: &Connection, ctx: &Ctx, ticket_id: &str, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.ticket.update")?;
        let (status, accum, clock_started, paused): (String, i64, i64, i64) = tx
            .query_row("SELECT status, sla_accum_minutes, clock_started_at, paused_flag FROM tickets WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, ticket_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("ticket '{}' not found", ticket_id))?;
        if status == "resolved" || status == "closed" {
            return Err(format!("ticket '{}' is already '{}'", ticket_id, status));
        }
        let business = if paused == 0 {
            business_minutes_between(tx, &ctx.tenant, clock_started, now)?
        } else {
            0
        };
        let new_accum = qadd(accum, business)?;
        tx.execute("UPDATE tickets SET sla_accum_minutes = ?3, status = 'resolved', resolved_at = ?4, paused_flag = 0 WHERE tenant_id = ?1 AND id = ?2", params![ctx.tenant, ticket_id, new_accum, now]).map_err(e)?;
        tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'resolve',?4,?5,?6)", params![ctx.tenant, new_id(), ticket_id, format!("final elapsed {} minutes", new_accum), ctx.user, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "ticket.resolved",
            ticket_id,
            &json!({"sla_accum_minutes": new_accum}),
            now,
        )?;
        Ok(
            json!({"id": ticket_id, "status": "resolved", "resolved_at": now, "sla_accum_minutes": new_accum}),
        )
    })
}

/// Links a knowledge reference to a ticket (`knowledge_link` event). [service.ticket.update]
pub fn link_knowledge(
    conn: &Connection,
    ctx: &Ctx,
    ticket_id: &str,
    reference: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.ticket.update")?;
        if reference.trim().is_empty() {
            return Err("field 'reference' must not be empty".to_string());
        }
        let exists: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM tickets WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ticket_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !exists {
            return Err(format!("ticket '{}' not found", ticket_id));
        }
        tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'knowledge_link',?4,?5,?6)", params![ctx.tenant, new_id(), ticket_id, reference, ctx.user, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "ticket.knowledge_linked",
            ticket_id,
            &json!({"reference": reference}),
            now,
        )?;
        Ok(json!({"id": ticket_id, "reference": reference}))
    })
}

/// Records a CSAT score (1-5) once, only on resolved tickets. [service.ticket.update]
pub fn record_csat(
    conn: &Connection,
    ctx: &Ctx,
    ticket_id: &str,
    score: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "service.ticket.update")?;
        if !(1..=5).contains(&score) {
            return Err("field 'score' must be between 1 and 5".to_string());
        }
        let (status, existing): (String, Option<i64>) = tx
            .query_row(
                "SELECT status, csat_score FROM tickets WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, ticket_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("ticket '{}' not found", ticket_id))?;
        if status != "resolved" {
            return Err(format!(
                "field 'status' must be 'resolved' to record CSAT, got '{}'",
                status
            ));
        }
        if existing.is_some() {
            return Err(format!("csat already recorded for ticket '{}'", ticket_id));
        }
        tx.execute(
            "UPDATE tickets SET csat_score = ?3 WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, ticket_id, score],
        )
        .map_err(e)?;
        tx.execute("INSERT INTO ticket_events (tenant_id, id, ticket_id, kind, detail, created_by, created_at) VALUES (?1,?2,?3,'csat',?4,?5,?6)", params![ctx.tenant, new_id(), ticket_id, score.to_string(), ctx.user, now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "ticket.csat_recorded",
            ticket_id,
            &json!({"score": score}),
            now,
        )?;
        Ok(json!({"id": ticket_id, "csat_score": score}))
    })
}

/// Read helper: SLA compliance across all tickets.
pub fn sla_compliance(conn: &Connection, tenant: &str) -> Res<Value> {
    let mut stmt = conn
        .prepare("SELECT t.status, t.sla_accum_minutes, t.breach_notified, p.resolve_minutes FROM tickets t JOIN sla_policies p ON p.tenant_id = t.tenant_id AND p.id = t.sla_policy_id WHERE t.tenant_id = ?1")
        .map_err(e)?;
    let rows = stmt
        .query_map(params![tenant], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(e)?;
    let tickets = rows.len() as i64;
    let mut resolved_count: i64 = 0;
    let mut within: i64 = 0;
    let mut breached: i64 = 0;
    for (status, accum, breach_notified, resolve_minutes) in rows {
        if breach_notified != 0 {
            breached += 1;
        }
        if status == "resolved" {
            resolved_count += 1;
            if accum <= resolve_minutes {
                within += 1;
            }
        }
    }
    let compliance_pct_kth = if resolved_count > 0 {
        i64::try_from((within as i128) * 100_000i128 / (resolved_count as i128)).map_err(e)?
    } else {
        0
    };
    Ok(
        json!({"tickets": tickets, "resolved_within_sla": within, "breached": breached, "compliance_pct_kth": compliance_pct_kth}),
    )
}
