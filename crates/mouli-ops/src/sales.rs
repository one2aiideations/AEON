//! Leads, opportunities, quotations, orders, deliveries, invoices and receipts.

use crate::common::{
    audit, e, idem_check, idem_store, in_tx, money_str, new_id, next_number, require_perm, ymd,
    Ctx, JournalEntry, JournalLine, Ledger, Res,
};
use crate::inventory;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

/// Parameters to create a customer.
pub struct CreateCustomer {
    /// Business code, unique per tenant.
    pub code: String,
    /// Legal name.
    pub name: String,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Credit limit in minor units; 0 means no credit extension.
    pub credit_limit_minor: i64,
}

/// Parameters to create a lead.
pub struct CreateLead {
    /// Contact person name.
    pub name: String,
    /// Company (optional).
    pub company: Option<String>,
    /// Contact detail (optional).
    pub contact: Option<String>,
    /// Owner user name.
    pub owner: String,
}

/// Parameters to create an opportunity.
pub struct CreateOpportunity {
    /// Optional source lead id.
    pub lead_id: Option<String>,
    /// Customer/prospect name.
    pub customer_name: String,
    /// Short title.
    pub title: String,
    /// Expected amount in minor units.
    pub amount_minor: i64,
    /// Owner user name.
    pub owner: String,
}

/// One quotation line.
#[derive(Clone)]
pub struct QuoteLineIn {
    /// Item id (optional for free-text lines).
    pub item_id: Option<String>,
    /// Description shown to the customer.
    pub description: String,
    /// Quantity in thousandths; must be > 0.
    pub qty_kth: i64,
    /// Unit price in minor units.
    pub unit_price_minor: i64,
    /// Line discount in thousandths of a percent (10_000 = 10%).
    pub discount_pct_kth: i64,
    /// Line tax in basis points (100 = 1%); 0 falls back to the quote header tax_bps.
    pub tax_bps: i64,
}

/// One sales order line.
#[derive(Clone)]
pub struct OrderLineIn {
    /// Item id.
    pub item_id: String,
    /// Quantity in thousandths.
    pub qty_kth: i64,
    /// Unit price in minor units.
    pub unit_price_minor: i64,
    /// Tax in basis points.
    pub tax_bps: i64,
}

/// One delivery line, referencing an order line.
pub struct DeliveryLineIn {
    /// 1-based sales order line number.
    pub order_line_no: i64,
    /// Quantity in thousandths to ship now.
    pub qty_kth: i64,
}

/// Parameters to create a quotation.
pub struct CreateQuotation {
    /// Customer id.
    pub customer_id: String,
    /// Optional source opportunity id.
    pub opportunity_id: Option<String>,
    /// Lines (at least one).
    pub lines: Vec<QuoteLineIn>,
    /// Flat header discount in minor units applied to the document total.
    pub discount_minor: i64,
    /// Header tax in basis points (used by lines whose own tax_bps is 0).
    pub tax_bps: i64,
    /// Days until expiry; must be positive.
    pub expiry_days: i64,
}

struct LineCalc {
    net: i128,
    tax: i128,
}

fn calc_line(
    qty_kth: i64,
    price: i64,
    disc_pct_kth: i64,
    tax_bps: i64,
) -> Result<LineCalc, String> {
    if qty_kth <= 0 {
        return Err("line quantity must be greater than zero".to_string());
    }
    if price < 0 {
        return Err("line unit price must not be negative".to_string());
    }
    if !(0..=100_000).contains(&disc_pct_kth) {
        return Err(
            "line discount must be between 0 and 100000 (thousandths of a percent)".to_string(),
        );
    }
    if !(0..=10_000).contains(&tax_bps) {
        return Err("line tax must be between 0 and 10000 basis points".to_string());
    }
    // Quantities are thousandths of a unit, so divide by 1000 (the same rule invoices use).
    let gross = (qty_kth as i128) * (price as i128) / 1000;
    let disc = gross * (disc_pct_kth as i128) / 100_000;
    let net = gross - disc;
    let tax = net * (tax_bps as i128) / 10_000;
    Ok(LineCalc { net, tax })
}

fn to_i64(v: i128, what: &str) -> Res<i64> {
    i64::try_from(v).map_err(|_| format!("{what} overflows 64-bit range"))
}

/// Create a customer.
pub fn create_customer(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &CreateCustomer,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.customers.create")?;
        if p.code.trim().is_empty() {
            return Err("field 'code' must not be empty".to_string());
        }
        if p.name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if p.currency.trim().len() != 3 {
            return Err("field 'currency' must be a 3-letter ISO code".to_string());
        }
        if p.credit_limit_minor < 0 {
            return Err("field 'credit_limit_minor' must not be negative".to_string());
        }
        let id = new_id();
        tx.execute("INSERT INTO customers(tenant_id,id,code,name,currency,credit_limit_minor) VALUES (?1,?2,?3,?4,?5,?6)",
            params![ctx.tenant, id, p.code.trim(), p.name.trim(), p.currency.trim().to_uppercase(), p.credit_limit_minor])
            .map_err(|err| format!("customer code '{}' cannot be created (duplicate?): {err}", p.code))?;
        audit(
            tx,
            &ctx.tenant,
            "customer.create",
            &id,
            &json!({"code": p.code, "name": p.name}),
            now,
        )?;
        Ok(json!({"id": id, "code": p.code.trim()}))
    })
}

/// Create a lead.
pub fn create_lead(conn: &rusqlite::Connection, ctx: &Ctx, p: &CreateLead, now: i64) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.leads.create")?;
        if p.name.trim().is_empty() {
            return Err("field 'name' must not be empty".to_string());
        }
        if p.owner.trim().is_empty() {
            return Err("field 'owner' must not be empty".to_string());
        }
        let id = new_id();
        tx.execute("INSERT INTO leads(tenant_id,id,name,company,contact,status,owner,created_at) VALUES (?1,?2,?3,?4,?5,'new',?6,?7)",
            params![ctx.tenant, id, p.name.trim(), p.company, p.contact, p.owner.trim(), now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "lead.create",
            &id,
            &json!({"name": p.name}),
            now,
        )?;
        Ok(json!({"id": id}))
    })
}

/// Mark a lead converted and open an opportunity for it.
pub fn convert_lead(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    lead_id: &str,
    title: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.leads.convert")?;
        if title.trim().is_empty() {
            return Err("field 'title' must not be empty".to_string());
        }
        let row = tx
            .query_row(
                "SELECT name, company, status FROM leads WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, lead_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(e)?;
        let (name, company, status) = row.ok_or_else(|| format!("lead '{lead_id}' not found"))?;
        if status != "new" && status != "qualified" {
            return Err(format!(
                "lead '{lead_id}' cannot be converted from status '{status}'"
            ));
        }
        let opp_id = new_id();
        tx.execute("INSERT INTO opportunities(tenant_id,id,lead_id,customer_name,title,amount_minor,stage,owner,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,0,'prospecting',?6,?7,?7)",
            params![ctx.tenant, opp_id, lead_id, company.unwrap_or(name), title.trim(), ctx.user, now]).map_err(e)?;
        tx.execute(
            "UPDATE leads SET status='converted' WHERE tenant_id=?1 AND id=?2",
            params![ctx.tenant, lead_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "lead.convert",
            &opp_id,
            &json!({"lead_id": lead_id, "title": title}),
            now,
        )?;
        Ok(json!({"opportunity_id": opp_id}))
    })
}

/// Create an opportunity directly.
pub fn create_opportunity(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &CreateOpportunity,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.opportunities.create")?;
        if p.customer_name.trim().is_empty() {
            return Err("field 'customer_name' must not be empty".to_string());
        }
        if p.title.trim().is_empty() {
            return Err("field 'title' must not be empty".to_string());
        }
        if p.amount_minor < 0 {
            return Err("field 'amount_minor' must not be negative".to_string());
        }
        let id = new_id();
        tx.execute("INSERT INTO opportunities(tenant_id,id,lead_id,customer_name,title,amount_minor,stage,owner,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,'prospecting',?7,?8,?8)",
            params![ctx.tenant, id, p.lead_id, p.customer_name.trim(), p.title.trim(), p.amount_minor, p.owner.trim(), now]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "opportunity.create",
            &id,
            &json!({"title": p.title, "amount_minor": p.amount_minor}),
            now,
        )?;
        Ok(json!({"id": id, "stage": "prospecting"}))
    })
}

/// Advance an opportunity. Moves are forward-only: prospecting -> proposal -> negotiation -> won|lost.
/// 'lost' requires close_reason. Terminal states cannot change.
pub fn set_opportunity_stage(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    opportunity_id: &str,
    stage: &str,
    close_reason: Option<&str>,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.opportunities.update")?;
        let current: Option<String> = tx
            .query_row(
                "SELECT stage FROM opportunities WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, opportunity_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        let current = current.ok_or_else(|| format!("opportunity '{opportunity_id}' not found"))?;
        if current == "won" || current == "lost" {
            return Err(format!("opportunity '{opportunity_id}' is already {current} (closed); no further stage changes"));
        }
        let rank = |s: &str| -> Option<i64> {
            match s {
                "prospecting" => Some(1),
                "proposal" => Some(2),
                "negotiation" => Some(3),
                "won" => Some(4),
                "lost" => Some(4),
                _ => None,
            }
        };
        let cr = rank(&current).ok_or_else(|| format!("stored stage '{current}' is invalid"))?;
        let nr = rank(stage).ok_or_else(|| format!("field 'stage' must be one of prospecting|proposal|negotiation|won|lost, got '{stage}'"))?;
        if nr < cr {
            return Err(format!(
                "opportunity cannot move backwards from '{current}' to '{stage}'"
            ));
        }
        if stage == "lost" && close_reason.map(|r| r.trim().is_empty()).unwrap_or(true) {
            return Err("field 'close_reason' is required when losing an opportunity".to_string());
        }
        tx.execute("UPDATE opportunities SET stage=?1, close_reason=COALESCE(?2, close_reason), updated_at=?3 WHERE tenant_id=?4 AND id=?5",
            params![stage, close_reason, now, ctx.tenant, opportunity_id]).map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "opportunity.stage",
            opportunity_id,
            &json!({"from": current, "to": stage, "close_reason": close_reason}),
            now,
        )?;
        Ok(json!({"id": opportunity_id, "stage": stage}))
    })
}

fn totals_of(
    lines: &[QuoteLineIn],
    header_discount: i64,
    header_tax_bps: i64,
) -> Result<(i64, i64, i64), String> {
    let mut net: i128 = 0;
    let mut tax: i128 = 0;
    for l in lines {
        let eff_tax = if l.tax_bps > 0 {
            l.tax_bps
        } else {
            header_tax_bps
        };
        let c = calc_line(l.qty_kth, l.unit_price_minor, l.discount_pct_kth, eff_tax)?;
        net += c.net;
        tax += c.tax;
    }
    if (header_discount as i128) > net {
        return Err("field 'discount_minor' exceeds the line subtotal".to_string());
    }
    Ok((
        to_i64(net, "subtotal")?,
        to_i64(tax, "tax")?,
        to_i64(net - header_discount as i128 + tax, "total")?,
    ))
}

/// Create a quotation (version 1, status 'draft'). Idempotent when idem_key is supplied.
pub fn create_quotation(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    p: &CreateQuotation,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    if p.lines.is_empty() {
        return Err("a quotation needs at least one line (field 'lines')".to_string());
    }
    if p.discount_minor < 0 {
        return Err("field 'discount_minor' must not be negative".to_string());
    }
    if !(0..=10_000).contains(&p.tax_bps) {
        return Err("field 'tax_bps' must be between 0 and 10000 basis points".to_string());
    }
    if p.expiry_days <= 0 {
        return Err("field 'expiry_days' must be positive".to_string());
    }
    let _ = totals_of(&p.lines, p.discount_minor, p.tax_bps)?;
    let payload = json!({"origin": "quotation", "customer_id": p.customer_id, "lines": p.lines.len(), "discount_minor": p.discount_minor, "tax_bps": p.tax_bps, "expiry_days": p.expiry_days});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.quotations.create")?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM customers WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, p.customer_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        if exists.is_none() {
            return Err(format!("customer '{}' not found", p.customer_id));
        }
        let company = ctx.company_or_default();
        let number = next_number(tx, &ctx.tenant, &company, "QT", now)?;
        let id = new_id();
        let expiry = now
            .checked_add(
                p.expiry_days
                    .checked_mul(86_400)
                    .ok_or_else(|| "expiry overflow".to_string())?,
            )
            .ok_or_else(|| "expiry overflow".to_string())?;
        tx.execute("INSERT INTO quotations(tenant_id,id,number,customer_id,opportunity_id,version,status,currency,discount_minor,tax_bps,expiry_at,created_by,created_at) \
                    SELECT ?1,?2,?3,?4,?5,1,'draft',currency,?6,?7,?8,?9,?10 FROM customers WHERE tenant_id=?1 AND id=?4",
            params![ctx.tenant, id, number, p.customer_id, p.opportunity_id, p.discount_minor, p.tax_bps, expiry, ctx.user, now]).map_err(e)?;
        for (idx, l) in p.lines.iter().enumerate() {
            let line_no = i64::try_from(idx + 1).map_err(|_| "too many lines".to_string())?;
            tx.execute("INSERT INTO quotation_lines(tenant_id,quotation_id,line_no,item_id,description,qty_kth,unit_price_minor,discount_pct_kth,tax_bps) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![ctx.tenant, id, line_no, l.item_id, l.description.trim(), l.qty_kth, l.unit_price_minor, l.discount_pct_kth, l.tax_bps]).map_err(e)?;
        }
        let (net, tax, total) = totals_of(&p.lines, p.discount_minor, p.tax_bps)?;
        let result = json!({"id": id, "number": number, "subtotal_minor": net, "tax_minor": tax, "total_minor": total, "expires_at": expiry});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(tx, &ctx.tenant, "quotation.create", &id, &result, now)?;
        Ok(result)
    })
}

/// Change quotation status: draft->sent, draft|sent->accepted|rejected, draft|sent->expired.
pub fn set_quotation_status(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    quotation_id: &str,
    status: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.quotations.update")?;
        let row = tx
            .query_row(
                "SELECT status, expiry_at FROM quotations WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, quotation_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(e)?;
        let (current, expiry) =
            row.ok_or_else(|| format!("quotation '{quotation_id}' not found"))?;
        let ok = matches!(
            (current.as_str(), status),
            ("draft", "sent")
                | ("draft", "accepted")
                | ("sent", "accepted")
                | ("draft", "rejected")
                | ("sent", "rejected")
                | ("draft", "expired")
                | ("sent", "expired")
        );
        if !ok {
            return Err(format!(
                "quotation cannot move from '{current}' to '{status}'"
            ));
        }
        if status == "accepted" && now > expiry {
            return Err(
                "quotation has expired; issue a new version instead of accepting".to_string(),
            );
        }
        tx.execute(
            "UPDATE quotations SET status=?1 WHERE tenant_id=?2 AND id=?3",
            params![status, ctx.tenant, quotation_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "quotation.status",
            quotation_id,
            &json!({"from": current, "to": status}),
            now,
        )?;
        Ok(json!({"id": quotation_id, "status": status}))
    })
}

fn order_total_from_lines(lines: &[OrderLineIn]) -> Result<(i64, i64), String> {
    let mut net: i128 = 0;
    let mut tax: i128 = 0;
    for l in lines {
        let c = calc_line(l.qty_kth, l.unit_price_minor, 0, l.tax_bps)?;
        net += c.net;
        tax += c.tax;
    }
    Ok((to_i64(net + tax, "order total")?, to_i64(tax, "tax")?))
}

fn insert_order(
    tx: &rusqlite::Connection,
    ctx: &Ctx,
    customer_id: &str,
    quotation_id: Option<&str>,
    currency: &str,
    lines: &[OrderLineIn],
    now: i64,
) -> Res<Value> {
    let company = ctx.company_or_default();
    let number = next_number(tx, &ctx.tenant, &company, "SO", now)?;
    let (total, _tax) = order_total_from_lines(lines)?;
    let id = new_id();
    tx.execute("INSERT INTO sales_orders(tenant_id,id,number,customer_id,quotation_id,status,currency,total_minor,created_by,created_at) VALUES (?1,?2,?3,?4,?5,'open',?6,?7,?8,?9)",
        params![ctx.tenant, id, number, customer_id, quotation_id, currency.to_uppercase(), total, ctx.user, now]).map_err(e)?;
    for (idx, l) in lines.iter().enumerate() {
        let line_no = i64::try_from(idx + 1).map_err(|_| "too many lines".to_string())?;
        tx.execute("INSERT INTO sales_order_lines(tenant_id,order_id,line_no,item_id,qty_kth,qty_delivered_kth,unit_price_minor,tax_bps) VALUES (?1,?2,?3,?4,?5,0,?6,?7)",
            params![ctx.tenant, id, line_no, l.item_id, l.qty_kth, l.unit_price_minor, l.tax_bps]).map_err(e)?;
    }
    Ok(json!({"id": id, "number": number, "total_minor": total}))
}

/// Create a sales order from explicit lines. Idempotent when idem_key is supplied.
pub fn create_sales_order(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    customer_id: &str,
    lines: &[OrderLineIn],
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    if lines.is_empty() {
        return Err("a sales order needs at least one line (field 'lines')".to_string());
    }
    for l in lines {
        calc_line(l.qty_kth, l.unit_price_minor, 0, l.tax_bps)?;
    }
    let payload = json!({"origin": "order", "customer_id": customer_id, "lines": lines.len(), "first_item": lines.first().map(|l| l.item_id.clone())});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.orders.create")?;
        let currency: Option<String> = tx
            .query_row(
                "SELECT currency FROM customers WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, customer_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        let currency = currency.ok_or_else(|| format!("customer '{customer_id}' not found"))?;
        let result = insert_order(tx, ctx, customer_id, None, &currency, lines, now)?;
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "sales_order.create",
            result.get("id").and_then(Value::as_str).unwrap_or(""),
            &result,
            now,
        )?;
        Ok(result)
    })
}

/// Convert an accepted quotation into a sales order (order total recomputed from lines).
pub fn create_order_from_quotation(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    quotation_id: &str,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let payload = json!({"origin": "order_from_quote", "quotation_id": quotation_id});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.orders.create")?;
        let row = tx.query_row("SELECT status, customer_id, expiry_at, currency FROM quotations WHERE tenant_id=?1 AND id=?2", params![ctx.tenant, quotation_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))).optional().map_err(e)?;
        let (status, customer_id, expiry, currency) =
            row.ok_or_else(|| format!("quotation '{quotation_id}' not found"))?;
        if status != "accepted" {
            return Err(format!(
                "quotation '{quotation_id}' must be accepted before conversion (status '{status}')"
            ));
        }
        if now > expiry {
            return Err("quotation expired; issue a new version instead of converting".to_string());
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM sales_orders WHERE tenant_id=?1 AND quotation_id=?2",
                params![ctx.tenant, quotation_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        if let Some(o) = existing {
            return Err(format!(
                "quotation '{quotation_id}' was already converted to order '{o}'"
            ));
        }
        let mut stmt = tx.prepare("SELECT item_id, qty_kth, unit_price_minor, tax_bps FROM quotation_lines WHERE tenant_id=?1 AND quotation_id=?2 ORDER BY line_no").map_err(e)?;
        let rows = stmt
            .query_map(params![ctx.tenant, quotation_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        let mut lines: Vec<OrderLineIn> = Vec::with_capacity(rows.len());
        for (item_id, qty, price, tax) in rows {
            let item = item_id.ok_or_else(|| "quotation contains a free-text line; create the order manually with create_sales_order".to_string())?;
            lines.push(OrderLineIn {
                item_id: item,
                qty_kth: qty,
                unit_price_minor: price,
                tax_bps: tax,
            });
        }
        let result = insert_order(
            tx,
            ctx,
            &customer_id,
            Some(quotation_id),
            &currency,
            &lines,
            now,
        )?;
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(
            tx,
            &ctx.tenant,
            "sales_order.create",
            result.get("id").and_then(Value::as_str).unwrap_or(""),
            &result,
            now,
        )?;
        Ok(result)
    })
}

fn remaining_qty(
    tx: &rusqlite::Connection,
    tenant: &str,
    order_id: &str,
    line_no: i64,
) -> Res<(i64, i64)> {
    tx.query_row("SELECT qty_kth, qty_delivered_kth FROM sales_order_lines WHERE tenant_id=?1 AND order_id=?2 AND line_no=?3",
        params![tenant, order_id, line_no], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional().map_err(e)?
        .ok_or_else(|| format!("order line {line_no} not found on order '{order_id}'"))
}

/// Create a draft delivery. Remaining quantities are validated; stock is checked at post time.
pub fn create_delivery(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    order_id: &str,
    warehouse_id: &str,
    lines: &[DeliveryLineIn],
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    if lines.is_empty() {
        return Err("a delivery needs at least one line (field 'lines')".to_string());
    }
    let payload = json!({"origin": "delivery", "order_id": order_id, "warehouse_id": warehouse_id, "lines": lines.len()});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.deliveries.create")?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM sales_orders WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, order_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        let status = status.ok_or_else(|| format!("sales order '{order_id}' not found"))?;
        if status != "open" && status != "partially_delivered" {
            return Err(format!(
                "order '{order_id}' cannot be delivered from status '{status}'"
            ));
        }
        for (idx, l) in lines.iter().enumerate() {
            if l.qty_kth <= 0 {
                return Err(format!(
                    "delivery line {}: quantity must be greater than zero",
                    idx + 1
                ));
            }
            let (ordered, delivered) = remaining_qty(tx, &ctx.tenant, order_id, l.order_line_no)?;
            if delivered
                .checked_add(l.qty_kth)
                .ok_or_else(|| "quantity overflow".to_string())?
                > ordered
            {
                return Err(format!("delivery line {}: exceeds the undelivered balance of order line {} (ordered {}.{:03}, delivered {}.{:03})",
                    idx + 1, l.order_line_no, ordered.div_euclid(1000), ordered.rem_euclid(1000), delivered.div_euclid(1000), delivered.rem_euclid(1000)));
            }
        }
        let company = ctx.company_or_default();
        let number = next_number(tx, &ctx.tenant, &company, "DN", now)?;
        let id = new_id();
        tx.execute("INSERT INTO deliveries(tenant_id,id,number,order_id,warehouse_id,status,created_by,created_at) VALUES (?1,?2,?3,?4,?5,'draft',?6,?7)",
            params![ctx.tenant, id, number, order_id, warehouse_id, ctx.user, now]).map_err(e)?;
        for (idx, l) in lines.iter().enumerate() {
            let line_no = i64::try_from(idx + 1).map_err(|_| "too many lines".to_string())?;
            let item_id: String = tx.query_row("SELECT item_id FROM sales_order_lines WHERE tenant_id=?1 AND order_id=?2 AND line_no=?3", params![ctx.tenant, order_id, l.order_line_no], |r| r.get(0)).map_err(e)?;
            tx.execute("INSERT INTO delivery_lines(tenant_id,delivery_id,line_no,order_line_no,item_id,qty_kth) VALUES (?1,?2,?3,?4,?5,?6)",
                params![ctx.tenant, id, line_no, l.order_line_no, item_id, l.qty_kth]).map_err(e)?;
        }
        let result = json!({"id": id, "number": number, "status": "draft"});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(tx, &ctx.tenant, "delivery.create", &id, &result, now)?;
        Ok(result)
    })
}

fn warehouse_of(tx: &rusqlite::Connection, tenant: &str, delivery_id: &str) -> Res<String> {
    tx.query_row(
        "SELECT warehouse_id FROM deliveries WHERE tenant_id=?1 AND id=?2",
        params![tenant, delivery_id],
        |r| r.get(0),
    )
    .map_err(e)
}

/// Post a draft delivery: availability pre-check, issue stock per line, bump delivered
/// quantities, flip the order to partially_delivered/delivered. All-or-nothing.
pub fn post_delivery(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    delivery_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.deliveries.post")?;
        let row = tx
            .query_row(
                "SELECT order_id, status FROM deliveries WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, delivery_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(e)?;
        let (order_id, status) =
            row.ok_or_else(|| format!("delivery '{delivery_id}' not found"))?;
        if status != "draft" {
            return Err(format!(
                "delivery '{delivery_id}' cannot be posted from status '{status}'"
            ));
        }
        let wh = warehouse_of(tx, &ctx.tenant, delivery_id)?;
        let mut stmt = tx.prepare("SELECT order_line_no, item_id, qty_kth FROM delivery_lines WHERE tenant_id=?1 AND delivery_id=?2 ORDER BY line_no").map_err(e)?;
        let lines = stmt
            .query_map(params![ctx.tenant, delivery_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        for (_oln, item_id, qty) in &lines {
            let have = inventory::on_hand(tx, &ctx.tenant, item_id, &wh)?;
            if have < *qty {
                return Err(format!(
                    "insufficient stock for item '{item_id}' (have {}.{:03}, need {}.{:03})",
                    have.div_euclid(1000),
                    have.rem_euclid(1000),
                    qty.div_euclid(1000),
                    qty.rem_euclid(1000)
                ));
            }
        }
        let mut total_cost: i64 = 0;
        for (oln, item_id, qty) in &lines {
            let out = inventory::issue_material(
                tx,
                ctx,
                item_id,
                &wh,
                *qty,
                "sales_order",
                delivery_id,
                now,
            )?;
            total_cost = total_cost
                .checked_add(out.total_cost_minor)
                .ok_or_else(|| "cost overflow".to_string())?;
            tx.execute("UPDATE sales_order_lines SET qty_delivered_kth = qty_delivered_kth + ?1 WHERE tenant_id=?2 AND order_id=?3 AND line_no=?4",
                params![qty, ctx.tenant, order_id, oln]).map_err(e)?;
        }
        let open_lines: i64 = tx.query_row("SELECT COUNT(*) FROM sales_order_lines WHERE tenant_id=?1 AND order_id=?2 AND qty_delivered_kth < qty_kth", params![ctx.tenant, order_id], |r| r.get(0)).map_err(e)?;
        let new_status = if open_lines == 0 {
            "delivered"
        } else {
            "partially_delivered"
        };
        tx.execute(
            "UPDATE sales_orders SET status=?1 WHERE tenant_id=?2 AND id=?3",
            params![new_status, ctx.tenant, order_id],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE deliveries SET status='posted', posted_at=?1 WHERE tenant_id=?2 AND id=?3",
            params![now, ctx.tenant, delivery_id],
        )
        .map_err(e)?;
        audit(
            tx,
            &ctx.tenant,
            "delivery.post",
            delivery_id,
            &json!({"order_id": order_id, "order_status": new_status, "total_cost_minor": total_cost}),
            now,
        )?;
        Ok(
            json!({"id": delivery_id, "order_id": order_id, "order_status": new_status, "cogs_minor": total_cost}),
        )
    })
}

/// Cancel a draft delivery. Posted deliveries cannot be cancelled.
pub fn cancel_delivery(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    delivery_id: &str,
    now: i64,
) -> Res<Value> {
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.deliveries.cancel")?;
        let n = tx.execute("UPDATE deliveries SET status='cancelled' WHERE tenant_id=?1 AND id=?2 AND status='draft'", params![ctx.tenant, delivery_id]).map_err(e)?;
        if n == 0 {
            return Err(format!(
                "delivery '{delivery_id}' not found or is not a draft"
            ));
        }
        audit(
            tx,
            &ctx.tenant,
            "delivery.cancel",
            delivery_id,
            &json!({}),
            now,
        )?;
        Ok(json!({"id": delivery_id, "status": "cancelled"}))
    })
}

/// Create the (single) invoice for an order, billing everything delivered so far.
/// Posts a balanced entry: Dr 1100 AR / Cr 4000 revenue, and Cr 2100 tax when tax > 0.
pub fn create_invoice_from_order(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    order_id: &str,
    ledger: &dyn Ledger,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    let payload = json!({"origin": "invoice", "order_id": order_id});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.invoices.create")?;
        let row = tx.query_row("SELECT customer_id, currency, status FROM sales_orders WHERE tenant_id=?1 AND id=?2", params![ctx.tenant, order_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).optional().map_err(e)?;
        let (customer_id, currency, ostatus) =
            row.ok_or_else(|| format!("sales order '{order_id}' not found"))?;
        if ostatus == "cancelled" {
            return Err(format!("order '{order_id}' is cancelled"));
        }
        let dup: Option<String> = tx
            .query_row(
                "SELECT id FROM invoices WHERE tenant_id=?1 AND order_id=?2 AND status != 'void'",
                params![ctx.tenant, order_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        if let Some(inv) = dup {
            return Err(format!("an invoice already exists for order '{order_id}' (invoice '{inv}'); this implementation keeps one invoice per order"));
        }
        let mut stmt = tx.prepare("SELECT line_no, item_id, qty_delivered_kth, unit_price_minor, tax_bps FROM sales_order_lines WHERE tenant_id=?1 AND order_id=?2 ORDER BY line_no").map_err(e)?;
        let rows = stmt
            .query_map(params![ctx.tenant, order_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(e)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(e)?;
        let mut net_total: i128 = 0;
        let mut tax_total: i128 = 0;
        let mut line_rows: Vec<(String, i64, i64, i64)> = Vec::new();
        for (_line_no, item_id, qty, price, tax_bps) in rows {
            if qty <= 0 {
                continue;
            }
            let net = (qty as i128) * (price as i128) / 1000;
            let tax = net * (tax_bps as i128) / 10_000;
            net_total += net;
            tax_total += tax;
            line_rows.push((item_id, qty, price, to_i64(net, "line total")?));
        }
        if line_rows.is_empty() {
            return Err(format!(
                "order '{order_id}' has no delivered quantities to invoice yet"
            ));
        }
        let net = to_i64(net_total, "invoice subtotal")?;
        let tax = to_i64(tax_total, "invoice tax")?;
        let gross = net
            .checked_add(tax)
            .ok_or_else(|| "invoice total overflow".to_string())?;
        if gross == 0 {
            return Err("invoice total is zero; check line prices".to_string());
        }
        let company = ctx.company_or_default();
        let number = next_number(tx, &ctx.tenant, &company, "INV", now)?;
        let id = new_id();
        tx.execute("INSERT INTO invoices(tenant_id,id,number,customer_id,order_id,currency,total_minor,tax_minor,status,created_by,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'posted',?9,?10)",
            params![ctx.tenant, id, number, customer_id, order_id, currency, gross, tax, ctx.user, now]).map_err(e)?;
        for (idx, (item_id, qty, price, line_total)) in line_rows.iter().enumerate() {
            let ln = i64::try_from(idx + 1).map_err(|_| "too many lines".to_string())?;
            tx.execute("INSERT INTO invoice_lines(tenant_id,invoice_id,line_no,item_id,description,qty_kth,unit_price_minor,total_minor) VALUES (?1,?2,?3,?4,'order line',?5,?6,?7)",
                params![ctx.tenant, id, ln, item_id, qty, price, line_total]).map_err(e)?;
        }
        let mut jlines = vec![
            JournalLine {
                account: "1100".to_string(),
                debit: gross,
                credit: 0,
            },
            JournalLine {
                account: "4000".to_string(),
                debit: 0,
                credit: net,
            },
        ];
        if tax > 0 {
            jlines.push(JournalLine {
                account: "2100".to_string(),
                debit: 0,
                credit: tax,
            });
        }
        let entry = JournalEntry {
            date: ymd(now),
            memo: format!("invoice {number}"),
            currency: currency.clone(),
            lines: jlines,
        };
        let ledger_entry = ledger.post(&ctx.tenant, &entry)?;
        let result = json!({"id": id, "number": number, "net_minor": net, "tax_minor": tax, "total_minor": gross, "ledger_entry": ledger_entry});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(tx, &ctx.tenant, "invoice.create", &id, &result, now)?;
        Ok(result)
    })
}

/// Record a receipt against an invoice. Overpayment is rejected. Posts Dr 1000 Cash / Cr 1100 AR.
pub fn record_receipt(
    conn: &rusqlite::Connection,
    ctx: &Ctx,
    invoice_id: &str,
    amount_minor: i64,
    method: &str,
    ledger: &dyn Ledger,
    idem_key: Option<&str>,
    now: i64,
) -> Res<Value> {
    if amount_minor <= 0 {
        return Err("field 'amount_minor' must be greater than zero".to_string());
    }
    if method.trim().is_empty() {
        return Err("field 'method' must not be empty".to_string());
    }
    let payload = json!({"origin": "receipt", "invoice_id": invoice_id, "amount_minor": amount_minor, "method": method});
    if let Some(k) = idem_key {
        if let Some(v) = idem_check(conn, &ctx.tenant, k, &payload)? {
            return Ok(v);
        }
    }
    in_tx(conn, |tx| {
        require_perm(tx, ctx, "sales.receipts.create")?;
        let row = tx
            .query_row(
                "SELECT total_minor, status, currency FROM invoices WHERE tenant_id=?1 AND id=?2",
                params![ctx.tenant, invoice_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(e)?;
        let (total, status, currency) =
            row.ok_or_else(|| format!("invoice '{invoice_id}' not found"))?;
        if status == "void" {
            return Err(format!("invoice '{invoice_id}' is void"));
        }
        let paid: i64 = tx.query_row("SELECT COALESCE(SUM(amount_minor),0) FROM receipts WHERE tenant_id=?1 AND invoice_id=?2", params![ctx.tenant, invoice_id], |r| r.get(0)).map_err(e)?;
        let outstanding = total
            .checked_sub(paid)
            .ok_or_else(|| "invoice totals overflow".to_string())?;
        if amount_minor > outstanding {
            return Err(format!(
                "receipt of {} exceeds the outstanding balance ({}) of invoice '{invoice_id}'",
                money_str(amount_minor),
                money_str(outstanding)
            ));
        }
        let id = new_id();
        tx.execute("INSERT INTO receipts(tenant_id,id,invoice_id,amount_minor,method,received_at,created_by) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![ctx.tenant, id, invoice_id, amount_minor, method.trim(), now, ctx.user]).map_err(e)?;
        let new_paid = paid
            .checked_add(amount_minor)
            .ok_or_else(|| "receipt total overflow".to_string())?;
        let new_status = if new_paid == total {
            "paid"
        } else {
            "partially_paid"
        };
        tx.execute(
            "UPDATE invoices SET status=?1 WHERE tenant_id=?2 AND id=?3",
            params![new_status, ctx.tenant, invoice_id],
        )
        .map_err(e)?;
        let entry = JournalEntry {
            date: ymd(now),
            memo: format!("receipt for invoice {invoice_id}"),
            currency,
            lines: vec![
                JournalLine {
                    account: "1000".to_string(),
                    debit: amount_minor,
                    credit: 0,
                },
                JournalLine {
                    account: "1100".to_string(),
                    debit: 0,
                    credit: amount_minor,
                },
            ],
        };
        let ledger_entry = ledger.post(&ctx.tenant, &entry)?;
        let result = json!({"id": id, "invoice_id": invoice_id, "invoice_status": new_status, "paid_minor": new_paid, "outstanding_minor": outstanding - amount_minor, "ledger_entry": ledger_entry});
        if let Some(k) = idem_key {
            idem_store(tx, &ctx.tenant, k, &payload, &result, now)?;
        }
        audit(tx, &ctx.tenant, "receipt.create", &id, &result, now)?;
        Ok(result)
    })
}

/// Open (not fully delivered, not cancelled) orders, newest first. No permission check.
pub fn open_orders(conn: &rusqlite::Connection, tenant: &str) -> Res<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT number, customer_id, status, total_minor, created_at FROM sales_orders WHERE tenant_id=?1 AND status IN ('open','partially_delivered') ORDER BY created_at DESC").map_err(e)?;
    let rows = stmt.query_map(params![tenant], |r| Ok(json!({"number": r.get::<_, String>(0)?, "customer_id": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?, "total_minor": r.get::<_, i64>(3)?, "created_at": r.get::<_, i64>(4)?}))).map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(e)?;
    Ok(rows)
}

/// Invoice rows in [from_ts, to_ts] for the sales-by-period report. No permission check.
pub fn invoices_between(
    conn: &rusqlite::Connection,
    tenant: &str,
    from_ts: i64,
    to_ts: i64,
) -> Res<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT number, customer_id, currency, total_minor, tax_minor, created_at FROM invoices WHERE tenant_id=?1 AND status != 'void' AND created_at >= ?2 AND created_at <= ?3 ORDER BY created_at").map_err(e)?;
    let rows = stmt.query_map(params![tenant, from_ts, to_ts], |r| Ok(json!({"number": r.get::<_, String>(0)?, "customer_id": r.get::<_, String>(1)?, "currency": r.get::<_, String>(2)?, "total_minor": r.get::<_, i64>(3)?, "tax_minor": r.get::<_, i64>(4)?, "created_at": r.get::<_, i64>(5)?}))).map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(e)?;
    Ok(rows)
}

/// Delivered-vs-ordered quantities per order line, for status displays. No permission check.
pub fn delivered_vs_ordered(
    conn: &rusqlite::Connection,
    tenant: &str,
    order_id: &str,
) -> Res<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT line_no, item_id, qty_kth, qty_delivered_kth FROM sales_order_lines WHERE tenant_id=?1 AND order_id=?2 ORDER BY line_no").map_err(e)?;
    let rows = stmt.query_map(params![tenant, order_id], |r| Ok(json!({"line_no": r.get::<_, i64>(0)?, "item_id": r.get::<_, String>(1)?, "ordered_kth": r.get::<_, i64>(2)?, "delivered_kth": r.get::<_, i64>(3)?}))).map_err(e)?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(e)?;
    Ok(rows)
}
