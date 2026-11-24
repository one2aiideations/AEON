//! Employees, leave accrual, attendance, payroll with proration and final settlement.

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

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 }.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9).rem_euclid(12);
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses and validates a YYYY-MM-DD date (leap-year aware) into (year, month, day).
pub fn parse_date(s: &str) -> Res<(i64, i64, i64)> {
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

fn parse_month(s: &str) -> Res<(i64, i64)> {
    let mut parts = s.split('-');
    let y: i64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("field 'period' must be in YYYY-MM format, got '{}'", s))?;
    let m: i64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("field 'period' must be in YYYY-MM format, got '{}'", s))?;
    if parts.next().is_some() {
        return Err(format!(
            "field 'period' must be in YYYY-MM format, got '{}'",
            s
        ));
    }
    if !(1..=12).contains(&m) {
        return Err(format!(
            "field 'period' month must be between 1 and 12, got {}",
            m
        ));
    }
    Ok((y, m))
}

/// Creates a department. [hr.department.create]
pub fn create_department(
    conn: &Connection,
    ctx: &Ctx,
    code: &str,
    name: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.department.create")?;
        if code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        if name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO departments (tenant_id, id, code, name) VALUES (?1,?2,?3,?4)",
            params![ctx.tenant, id, code, name],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.department_created",
            &id,
            &json!({"code": code}),
            now,
        )?;
        Ok(json!({"id": id, "code": code}))
    })
}

/// Creates a position in a department. [hr.position.create]
pub fn create_position(
    conn: &Connection,
    ctx: &Ctx,
    code: &str,
    title: &str,
    department_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.position.create")?;
        if code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        if title.trim().is_empty() {
            return Err("field 'title' must not be empty".to_string());
        }
        let ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, department_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !ok {
            return Err(format!(
                "field 'department_id' references unknown department '{}'",
                department_id
            ));
        }
        let id = new_id();
        tx.execute("INSERT INTO positions (tenant_id, id, code, title, department_id) VALUES (?1,?2,?3,?4,?5)", params![ctx.tenant, id, code, title, department_id]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.position_created",
            &id,
            &json!({"code": code}),
            now,
        )?;
        Ok(json!({"id": id, "code": code}))
    })
}

/// Creates an active employee; validates `hire_date` (YYYY-MM-DD, leap-year aware). [hr.employee.create]
pub fn create_employee(
    conn: &Connection,
    ctx: &Ctx,
    code: &str,
    first_name: &str,
    last_name: &str,
    department_id: &str,
    position_id: Option<&str>,
    hire_date: &str,
    base_salary_minor: i64,
    currency: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.employee.create")?;
        if code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        parse_date(hire_date)?;
        if base_salary_minor < 0 {
            return Err(
                "field 'base_salary_minor' must be greater than or equal to zero".to_string(),
            );
        }
        let ok: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, department_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if !ok {
            return Err(format!(
                "field 'department_id' references unknown department '{}'",
                department_id
            ));
        }
        if let Some(pid) = position_id {
            let ok2: bool = tx
                .query_row(
                    "SELECT COUNT(*) FROM positions WHERE tenant_id = ?1 AND id = ?2",
                    params![ctx.tenant, pid],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(e)?
                > 0;
            if !ok2 {
                return Err(format!(
                    "field 'position_id' references unknown position '{}'",
                    pid
                ));
            }
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO employees (tenant_id, id, code, first_name, last_name, department_id, position_id, hire_date, base_salary_minor, currency, status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'active')",
            params![ctx.tenant, id, code, first_name, last_name, department_id, position_id, hire_date, base_salary_minor, currency],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.employee_created",
            &id,
            &json!({"code": code, "hire_date": hire_date}),
            now,
        )?;
        Ok(json!({"id": id, "code": code, "status": "active"}))
    })
}

/// Creates a leave type with a monthly accrual (thousandths of a day). [hr.leave_type.create]
pub fn create_leave_type(
    conn: &Connection,
    ctx: &Ctx,
    code: &str,
    name: &str,
    accrual_per_month_kth: i64,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.leave_type.create")?;
        if code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        if name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if accrual_per_month_kth < 0 {
            return Err(
                "field 'accrual_per_month_kth' must be greater than or equal to zero".to_string(),
            );
        }
        let id = new_id();
        tx.execute("INSERT INTO leave_types (tenant_id, id, code, name, accrual_per_month_kth) VALUES (?1,?2,?3,?4,?5)", params![ctx.tenant, id, code, name, accrual_per_month_kth]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.leave_type_created",
            &id,
            &json!({"code": code}),
            now,
        )?;
        Ok(json!({"id": id, "code": code}))
    })
}

/// Accrues monthly leave for active employees hired on or before month end; idempotent per month. [hr.leave.accrue]
pub fn accrue_leave(conn: &Connection, ctx: &Ctx, month: &str, now: i64) -> Res<Value> {
    let (y, m) = parse_month(month)?;
    let dim = days_in_month(y, m)?;
    let last_day = format!("{:04}-{:02}-{:02}", y, m, dim);
    let key = format!("leave-accrual-{}", month);
    let payload = json!({"month": month});
    if let Some(v) = idem_check(conn, &ctx.tenant, &key, &payload)? {
        return Ok(v);
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.leave.accrue")?;
        let mut emp_stmt = tx
            .prepare("SELECT id FROM employees WHERE tenant_id = ?1 AND status = 'active' AND hire_date <= ?2")
            .map_err(e)?;
        let employees = emp_stmt
            .query_map(params![ctx.tenant, last_day], |r| r.get::<_, String>(0))
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        let mut lt_stmt = tx
            .prepare("SELECT id, accrual_per_month_kth FROM leave_types WHERE tenant_id = ?1")
            .map_err(e)?;
        let leave_types = lt_stmt
            .query_map(params![ctx.tenant], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        for (lt_id, accrual) in &leave_types {
            if *accrual == 0 {
                continue;
            }
            for emp_id in &employees {
                tx.execute(
                    "INSERT INTO leave_balances (tenant_id, employee_id, leave_type_id, balance_kth) VALUES (?1,?2,?3,?4) ON CONFLICT(tenant_id, employee_id, leave_type_id) DO UPDATE SET balance_kth = balance_kth + excluded.balance_kth",
                    params![ctx.tenant, emp_id, lt_id, accrual],
                )
                .map_err(e)?;
            }
        }
        let result =
            json!({"month": month, "employees": employees.len(), "leave_types": leave_types.len()});
        idem_store(tx, &ctx.tenant, &key, &payload, &result, now)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.leave_accrued",
            "leave-accrual",
            &payload,
            now,
        )?;
        Ok(result)
    })
}

/// Takes leave; errors when the balance is insufficient; writes attendance with present=0. [hr.leave.take]
pub fn take_leave(
    conn: &Connection,
    ctx: &Ctx,
    employee_id: &str,
    leave_type_id: &str,
    days_kth: i64,
    date: &str,
    now: i64,
) -> Res<Value> {
    parse_date(date)?;
    if days_kth <= 0 {
        return Err("field 'days_kth' must be greater than zero".to_string());
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.leave.take")?;
        let balance: i64 = tx
            .query_row(
                "SELECT COALESCE((SELECT balance_kth FROM leave_balances WHERE tenant_id = ?1 AND employee_id = ?2 AND leave_type_id = ?3), 0)",
                params![ctx.tenant, employee_id, leave_type_id],
                |r| r.get(0),
            )
            .map_err(e)?;
        if days_kth > balance {
            return Err(format!("insufficient leave balance for employee '{}': balance is {} thousandths of a day, requested {}", employee_id, balance, days_kth));
        }
        let new_balance = qsub(balance, days_kth)?;
        tx.execute(
            "INSERT INTO leave_balances (tenant_id, employee_id, leave_type_id, balance_kth) VALUES (?1,?2,?3,?4) ON CONFLICT(tenant_id, employee_id, leave_type_id) DO UPDATE SET balance_kth = ?4",
            params![ctx.tenant, employee_id, leave_type_id, new_balance],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO attendance (tenant_id, employee_id, date, present, hours_kth) VALUES (?1,?2,?3,0,0) ON CONFLICT(tenant_id, employee_id, date) DO UPDATE SET present = 0, hours_kth = 0",
            params![ctx.tenant, employee_id, date],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.leave_taken",
            employee_id,
            &json!({"leave_type_id": leave_type_id, "days_kth": days_kth, "date": date}),
            now,
        )?;
        Ok(
            json!({"employee_id": employee_id, "leave_type_id": leave_type_id, "days_kth": days_kth, "balance_kth": new_balance}),
        )
    })
}

/// Marks attendance for an employee on a date (upsert). [hr.attendance.mark]
pub fn mark_attendance(
    conn: &Connection,
    ctx: &Ctx,
    employee_id: &str,
    date: &str,
    present: bool,
    hours_kth: i64,
    now: i64,
) -> Res<Value> {
    parse_date(date)?;
    if hours_kth < 0 {
        return Err("field 'hours_kth' must be greater than or equal to zero".to_string());
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.attendance.mark")?;
        tx.execute(
            "INSERT INTO attendance (tenant_id, employee_id, date, present, hours_kth) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(tenant_id, employee_id, date) DO UPDATE SET present = excluded.present, hours_kth = excluded.hours_kth",
            params![ctx.tenant, employee_id, date, if present { 1 } else { 0 }, hours_kth],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.attendance_marked",
            employee_id,
            &json!({"date": date, "present": present, "hours_kth": hours_kth}),
            now,
        )?;
        Ok(
            json!({"employee_id": employee_id, "date": date, "present": present, "hours_kth": hours_kth}),
        )
    })
}

/// Parameters for creating a payroll rule.
pub struct CreatePayrollRule {
    pub code: String,
    pub name: String,
    pub kind: String,
    pub rate_kth: Option<i64>,
    pub amount_minor: Option<i64>,
    pub employer_side: bool,
    pub order_idx: i64,
}

/// Creates a payroll rule; percent kinds use `rate_kth` (thousandths of a percent), `fixed_minor` uses `amount_minor`. [hr.rules.create]
pub fn create_payroll_rule(
    conn: &Connection,
    ctx: &Ctx,
    p: &CreatePayrollRule,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.rules.create")?;
        match p.kind.as_str() {
            "percent_of_gross" | "percent_of_basic" => match p.rate_kth {
                Some(r) if r >= 0 => {}
                Some(_) => return Err("field 'rate_kth' must be greater than or equal to zero".to_string()),
                None => return Err(format!("field 'rate_kth' is required for rule kind '{}'", p.kind)),
            },
            "fixed_minor" => match p.amount_minor {
                Some(a) if a >= 0 => {}
                Some(_) => return Err("field 'amount_minor' must be greater than or equal to zero".to_string()),
                None => return Err("field 'amount_minor' is required for rule kind 'fixed_minor'".to_string()),
            },
            other => return Err(format!("field 'kind' must be 'percent_of_gross', 'percent_of_basic' or 'fixed_minor', got '{}'", other)),
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO payroll_rules (tenant_id, id, code, name, kind, rate_kth, amount_minor, employer_side, order_idx) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![ctx.tenant, id, p.code, p.name, p.kind, p.rate_kth, p.amount_minor, if p.employer_side { 1 } else { 0 }, p.order_idx],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "hr.payroll_rule_created",
            &id,
            &json!({"code": p.code, "kind": p.kind}),
            now,
        )?;
        Ok(json!({"id": id, "code": p.code, "kind": p.kind}))
    })
}

/// Runs payroll for a YYYY-MM period; one run per period unless replayed with the same idempotency key. [hr.payroll.run]
pub fn run_payroll(
    conn: &Connection,
    ctx: &Ctx,
    period: &str,
    ledger: &dyn Ledger,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let (y, m) = parse_month(period)?;
    let dim = days_in_month(y, m)?;
    let month_start = format!("{:04}-{:02}-01", y, m);
    let payload = json!({"period": period});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.payroll.run")?;
        let existing: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM payroll_runs WHERE tenant_id = ?1 AND period = ?2",
                params![ctx.tenant, period],
                |r| r.get::<_, i64>(0),
            )
            .map_err(e)?
            > 0;
        if existing {
            return Err(format!(
                "a payroll run for period '{}' already exists",
                period
            ));
        }
        // Collect employees: active ones, plus terminated ones whose latest
        // settlement date falls in or after the period start.
        let mut employees: Vec<(String, String, i64, String, Option<String>)> = Vec::new();
        {
            let mut stmt = tx
                .prepare("SELECT id, hire_date, base_salary_minor, currency FROM employees WHERE tenant_id = ?1 AND status = 'active'")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        None::<String>,
                    ))
                })
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            employees.extend(rows);
        }
        {
            let mut stmt = tx
                .prepare("SELECT e.id, e.hire_date, e.base_salary_minor, e.currency, (SELECT s.settlement_date FROM settlements s WHERE s.tenant_id = e.tenant_id AND s.employee_id = e.id ORDER BY s.settlement_date DESC LIMIT 1) AS sdate FROM employees e WHERE e.tenant_id = ?1 AND e.status = 'terminated' AND (SELECT s.settlement_date FROM settlements s WHERE s.tenant_id = e.tenant_id AND s.employee_id = e.id ORDER BY s.settlement_date DESC LIMIT 1) >= ?2")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![ctx.tenant, month_start], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(e)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(e)?;
            employees.extend(rows);
        }
        let mut rule_stmt = tx
            .prepare("SELECT code, kind, rate_kth, amount_minor, employer_side FROM payroll_rules WHERE tenant_id = ?1 ORDER BY order_idx ASC, code ASC")
            .map_err(e)?;
        let rules = rule_stmt
            .query_map(params![ctx.tenant], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        let month_start_d = days_from_civil(y, m, 1);
        let month_end_d = days_from_civil(y, m, dim);
        let company = ctx.company.as_deref().unwrap_or("default");
        let number = next_number(tx, &ctx.tenant, company, "PRN", now)?;
        let run_id = new_id();
        tx.execute(
            "INSERT INTO payroll_runs (tenant_id, id, number, period, status, created_by, created_at, posted_at) VALUES (?1,?2,?3,?4,'posted',?5,?6,?6)",
            params![ctx.tenant, run_id, number, period, ctx.user, now],
        )
        .map_err(e)?;
        let mut by_currency: HashMap<String, (i64, i64, i64, i64, i64)> = HashMap::new(); // gross, deductions, employer contributions, net, count
        for (emp_id, hire_date, base_salary, currency, settlement_date) in &employees {
            let (hy, hm, hd) = parse_date(hire_date)?;
            let hire_d = days_from_civil(hy, hm, hd);
            let from = hire_d.max(month_start_d);
            let to = match settlement_date {
                Some(s) => {
                    let (sy2, sm2, sd2) = parse_date(s)?;
                    days_from_civil(sy2, sm2, sd2).min(month_end_d)
                }
                None => month_end_d,
            };
            let employed_days = if to >= from { to - from + 1 } else { 0 };
            let mut proration = employed_days
                .checked_mul(1000)
                .ok_or("proration arithmetic overflow")?
                / dim;
            if proration > 1000 {
                proration = 1000;
            }
            let gross = mmul_qty(*base_salary, proration)?;
            let mut deductions: i64 = 0;
            let mut employer_contribs: i64 = 0;
            let mut slip_lines: Vec<(String, String, i64)> = Vec::with_capacity(rules.len());
            for (code, kind, rate_kth, amount_minor, employer_side) in &rules {
                let amount: i64 = match kind.as_str() {
                    "percent_of_gross" | "percent_of_basic" => {
                        let rate = rate_kth.ok_or_else(|| {
                            format!("field 'rate_kth' is required for rule '{}'", code)
                        })?;
                        let v = (gross as i128)
                            .checked_mul(rate as i128)
                            .ok_or("rule arithmetic overflow")?
                            + 50_000i128;
                        i64::try_from(v / 100_000i128).map_err(e)?
                    }
                    "fixed_minor" => amount_minor.ok_or_else(|| {
                        format!("field 'amount_minor' is required for rule '{}'", code)
                    })?,
                    other => return Err(format!("field 'kind' has unsupported value '{}'", other)),
                };
                let side = if *employer_side != 0 {
                    "employer"
                } else {
                    "deduction"
                };
                if *employer_side != 0 {
                    employer_contribs = madd(employer_contribs, amount)?;
                } else {
                    deductions = madd(deductions, amount)?;
                }
                slip_lines.push((code.clone(), side.to_string(), amount));
            }
            let net = msub(gross, deductions)?;
            let employer_cost = madd(gross, employer_contribs)?;
            let slip_id = new_id();
            tx.execute(
                "INSERT INTO payslips (tenant_id, id, run_id, employee_id, gross_minor, deductions_minor, employer_cost_minor, net_minor, proration_kth) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![ctx.tenant, slip_id, run_id, emp_id, gross, deductions, employer_cost, net, proration],
            )
            .map_err(e)?;
            for (code, side, amount) in &slip_lines {
                tx.execute(
                    "INSERT INTO payslip_lines (tenant_id, payslip_id, rule_code, side, amount_minor) VALUES (?1,?2,?3,?4,?5)",
                    params![ctx.tenant, slip_id, code, side, amount],
                )
                .map_err(e)?;
            }
            let entry = by_currency
                .entry(currency.clone())
                .or_insert((0, 0, 0, 0, 0));
            entry.0 = madd(entry.0, gross)?;
            entry.1 = madd(entry.1, deductions)?;
            entry.2 = madd(entry.2, employer_contribs)?;
            entry.3 = madd(entry.3, net)?;
            entry.4 = entry.4.checked_add(1).ok_or("employee count overflow")?;
        }
        let mut currency_json = Vec::with_capacity(by_currency.len());
        for (currency, (gross, ded, er, net, count)) in &by_currency {
            let mut lines = Vec::new();
            if *gross > 0 {
                lines.push(JournalLine {
                    account: "6000".to_string(),
                    debit: *gross,
                    credit: 0,
                });
            }
            if *er > 0 {
                lines.push(JournalLine {
                    account: "6100".to_string(),
                    debit: *er,
                    credit: 0,
                });
            }
            if *net > 0 {
                lines.push(JournalLine {
                    account: "2200".to_string(),
                    debit: 0,
                    credit: *net,
                });
            }
            let ded_plus_er = madd(*ded, *er)?;
            if ded_plus_er > 0 {
                lines.push(JournalLine {
                    account: "2300".to_string(),
                    debit: 0,
                    credit: ded_plus_er,
                });
            }
            if !lines.is_empty() {
                ledger.post(
                    &ctx.tenant,
                    &JournalEntry {
                        date: today(now),
                        memo: format!("Payroll {}", period),
                        currency: currency.clone(),
                        lines,
                    },
                )?;
            }
            currency_json.push(json!({"currency": currency, "gross_minor": gross, "deductions_minor": ded, "employer_contributions_minor": er, "net_minor": net, "employees": count}));
        }
        let result = json!({"id": run_id, "number": number, "period": period, "status": "posted", "employees": employees.len(), "currencies": currency_json});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "hr.payroll_run",
            &run_id,
            &json!({"period": period, "number": number, "employees": employees.len()}),
            now,
        )?;
        Ok(result)
    })
}

/// Runs a final settlement for an active employee and terminates them. [hr.settlement.run]
pub fn final_settlement(
    conn: &Connection,
    ctx: &Ctx,
    employee_id: &str,
    settlement_date: &str,
    notice_recovery_minor: i64,
    ledger: &dyn Ledger,
    currency: &str,
    now: i64,
) -> Res<Value> {
    let (sy, sm, sd) = parse_date(settlement_date)?;
    if notice_recovery_minor < 0 {
        return Err(
            "field 'notice_recovery_minor' must be greater than or equal to zero".to_string(),
        );
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "hr.settlement.run")?;
        let (hire_date, base_salary, status): (String, i64, String) = tx
            .query_row(
                "SELECT hire_date, base_salary_minor, status FROM employees WHERE tenant_id = ?1 AND id = ?2",
                params![ctx.tenant, employee_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("employee '{}' not found", employee_id))?;
        if status != "active" {
            return Err(format!(
                "field 'status' must be 'active' to run a settlement, got '{}'",
                status
            ));
        }
        let (hy, hm, hd) = parse_date(&hire_date)?;
        let total_leave: i64 = tx
            .query_row("SELECT COALESCE(SUM(balance_kth), 0) FROM leave_balances WHERE tenant_id = ?1 AND employee_id = ?2", params![ctx.tenant, employee_id], |r| r.get(0))
            .map_err(e)?;
        let payout_num = (total_leave as i128)
            .checked_mul(base_salary as i128)
            .ok_or("leave payout arithmetic overflow")?;
        let leave_payout = i64::try_from((payout_num + 15_000i128) / 30_000i128).map_err(e)?;
        let mut years = sy - hy;
        if (sm, sd) < (hm, hd) {
            years -= 1;
        }
        let gratuity: i64 = if years >= 5 {
            let g = (base_salary as i128)
                .checked_mul(15)
                .and_then(|v| v.checked_mul(years as i128))
                .ok_or("gratuity arithmetic overflow")?;
            i64::try_from((g + 13) / 26).map_err(e)?
        } else {
            0
        };
        let gross_component = madd(leave_payout, gratuity)?;
        let net = msub(gross_component, notice_recovery_minor)?;
        if net < 0 {
            return Err(format!(
                "field 'notice_recovery_minor' ({}) exceeds leave payout plus gratuity ({})",
                notice_recovery_minor, gross_component
            ));
        }
        let id = new_id();
        tx.execute(
            "INSERT INTO settlements (tenant_id, id, employee_id, settlement_date, leave_payout_minor, gratuity_minor, notice_recovery_minor, net_minor, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![ctx.tenant, id, employee_id, settlement_date, leave_payout, gratuity, notice_recovery_minor, net, now],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE employees SET status = 'terminated' WHERE tenant_id = ?1 AND id = ?2",
            params![ctx.tenant, employee_id],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE leave_balances SET balance_kth = 0 WHERE tenant_id = ?1 AND employee_id = ?2",
            params![ctx.tenant, employee_id],
        )
        .map_err(e)?;
        if gross_component > 0 {
            let mut lines = vec![JournalLine {
                account: "6000".to_string(),
                debit: gross_component,
                credit: 0,
            }];
            if net > 0 {
                lines.push(JournalLine {
                    account: "2200".to_string(),
                    debit: 0,
                    credit: net,
                });
            }
            if notice_recovery_minor > 0 {
                lines.push(JournalLine {
                    account: "2300".to_string(),
                    debit: 0,
                    credit: notice_recovery_minor,
                });
            }
            ledger.post(
                &ctx.tenant,
                &JournalEntry {
                    date: settlement_date.to_string(),
                    memo: format!("Final settlement {}", employee_id),
                    currency: currency.to_string(),
                    lines,
                },
            )?;
        }
        audit(
            tx,
            &ctx.tenant,
            "hr.settlement_run",
            &id,
            &json!({"employee_id": employee_id, "net_minor": net}),
            now,
        )?;
        Ok(
            json!({"id": id, "employee_id": employee_id, "leave_payout_minor": leave_payout, "gratuity_minor": gratuity, "notice_recovery_minor": notice_recovery_minor, "net_minor": net}),
        )
    })
}

/// Read helper: aggregated payroll figures for a period.
pub fn payroll_summary(conn: &Connection, tenant: &str, period: &str) -> Res<Value> {
    let row = conn
        .query_row(
            "SELECT COALESCE(SUM(p.gross_minor), 0), COALESCE(SUM(p.deductions_minor), 0), COALESCE(SUM(p.employer_cost_minor), 0), COALESCE(SUM(p.net_minor), 0), COUNT(*) FROM payslips p JOIN payroll_runs r ON r.tenant_id = p.tenant_id AND r.id = p.run_id WHERE r.tenant_id = ?1 AND r.period = ?2",
            params![tenant, period],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)),
        )
        .map_err(e)?;
    Ok(
        json!({"period": period, "gross_minor": row.0, "deductions_minor": row.1, "employer_cost_minor": row.2, "net_minor": row.3, "employees": row.4}),
    )
}
