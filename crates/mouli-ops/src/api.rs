//! REST-style router: maps `(method, path, body)` to the operations of the other modules.

use crate::common::*;
use crate::{admin_reports, hr, inventory, manufacturing, procurement, projects_service, sales};
use rusqlite::Connection;
use serde_json::{json, Value};

pub struct Api<'a> {
    pub conn: &'a Connection,
    pub ledger: &'a dyn Ledger,
    pub clock: &'a dyn Clock,
}

impl<'a> Api<'a> {
    pub fn new(conn: &'a Connection, ledger: &'a dyn Ledger, clock: &'a dyn Clock) -> Self {
        Api {
            conn,
            ledger,
            clock,
        }
    }

    pub fn handle(&self, method: &str, path: &str, body: &Value, ctx: &Ctx) -> Res<Value> {
        let m = method.to_uppercase();
        let (p, query) = match path.split_once('?') {
            Some((p, q)) => (p, q),
            None => (path, ""),
        };
        let mut merged = body.as_object().cloned().unwrap_or_default();
        for (k, v) in parse_query(query) {
            merged.entry(k).or_insert(v);
        }
        let b = Value::Object(merged);
        let now = self.clock.now();
        if p == "/health" {
            return if m == "GET" {
                Ok(json!({"status": "ok"}))
            } else {
                Err(format!("method not allowed: {} {}", m, p))
            };
        }
        if let Some(r) = self.admin_route(&m, p, &b, ctx, now) {
            return r;
        }
        if let Some(r) = self.inventory_route(&m, p, &b, ctx, now) {
            return r;
        }
        if let Some(r) = self.sales_route(&m, p, &b, ctx, now) {
            return r;
        }
        if let Some(r) = self.procurement_route(&m, p, &b, ctx, now) {
            return r;
        }
        if let Some(r) = self.manufacturing_route(&m, p, &b, ctx, now) {
            return r;
        }
        if let Some(r) = self.hr_route(&m, p, &b, ctx, now) {
            return r;
        }
        if let Some(r) = self.projects_route(&m, p, &b, ctx, now) {
            return r;
        }
        Err(format!("not found: {} {}", m, p))
    }

    fn admin_route(&self, m: &str, p: &str, b: &Value, ctx: &Ctx, now: i64) -> Option<Res<Value>> {
        if let Some(name) = p.strip_prefix("/reports/") {
            let r = if m == "GET" {
                admin_reports::run_report(self.conn, ctx, name, b)
            } else {
                Err(format!("method not allowed: {} {}", m, p))
            };
            return Some(r);
        }
        let known = matches!(
            p,
            "/admin/warehouses"
                | "/admin/roles/grant"
                | "/admin/roles/revoke"
                | "/admin/approval-limits"
                | "/notifications/read"
                | "/notifications"
                | "/audit/verify"
                | "/attachments"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/admin/warehouses") => (|| {
                let code = jstr(b, "code")?;
                let name = jstr(b, "name")?;
                admin_reports::create_warehouse(self.conn, ctx, &code, &name, now)
            })(),
            ("GET", "/admin/warehouses") => {
                admin_reports::list_warehouses(self.conn, &ctx.tenant).map(Value::Array)
            }
            ("POST", "/admin/roles/grant") => (|| {
                let role = jstr(b, "role")?;
                let permission = jstr(b, "permission")?;
                admin_reports::grant_permission(self.conn, ctx, &role, &permission, now)
            })(),
            ("POST", "/admin/roles/revoke") => (|| {
                let role = jstr(b, "role")?;
                let permission = jstr(b, "permission")?;
                admin_reports::revoke_permission(self.conn, ctx, &role, &permission, now)
            })(),
            ("POST", "/admin/approval-limits") => (|| {
                let role = jstr(b, "role")?;
                let limit_minor = jint(b, "limit_minor")?;
                admin_reports::set_approval_limit(self.conn, ctx, &role, limit_minor, now)
            })(),
            ("POST", "/notifications/read") => (|| {
                let notification_id = jstr(b, "notification_id")?;
                admin_reports::mark_notification_read(self.conn, ctx, &notification_id, now)
            })(),
            ("GET", "/notifications") => (|| {
                let user = jstr(b, "user")?;
                let unread_only = jbool(b, "unread_only");
                let limit = jopt_int(b, "limit").unwrap_or(50);
                admin_reports::list_notifications(self.conn, &ctx.tenant, &user, unread_only, limit)
                    .map(Value::Array)
            })(),
            ("GET", "/audit/verify") => admin_reports::audit_integrity(self.conn, ctx),
            ("POST", "/attachments") => in_tx(self.conn, |tx| {
                let entity_type = jstr(b, "entity_type")?;
                let entity_id = jstr(b, "entity_id")?;
                let name = jstr(b, "name")?;
                let hash = jstr(b, "hash")?;
                let size_bytes = jint(b, "size_bytes")?;
                let id = add_attachment(
                    tx,
                    ctx,
                    &entity_type,
                    &entity_id,
                    &name,
                    &hash,
                    size_bytes,
                    now,
                )?;
                audit(
                    tx,
                    &ctx.tenant,
                    "attachment.add",
                    &id,
                    &json!({
                        "entity_type": entity_type,
                        "entity_id": entity_id,
                        "name": name,
                        "hash": hash,
                        "size_bytes": size_bytes,
                    }),
                    now,
                )?;
                Ok(json!({"id": id}))
            }),
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }

    fn inventory_route(
        &self,
        m: &str,
        p: &str,
        b: &Value,
        ctx: &Ctx,
        now: i64,
    ) -> Option<Res<Value>> {
        let known = matches!(
            p,
            "/inventory/items"
                | "/inventory/receive"
                | "/inventory/issue"
                | "/inventory/adjust"
                | "/inventory/transfer"
                | "/inventory/reserve"
                | "/inventory/reservations/status"
                | "/inventory/serials"
                | "/inventory/cycle-counts"
                | "/inventory/cycle-counts/record"
                | "/inventory/cycle-counts/post"
                | "/inventory/stock"
                | "/inventory/reorder"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/inventory/items") => (|| {
                let p = inventory::CreateItem {
                    code: jstr(b, "code")?,
                    name: jstr(b, "name")?,
                    uom: jstr(b, "uom")?,
                    valuation_method: jstr(b, "valuation_method")?,
                    reorder_point_kth: jopt_int(b, "reorder_point_kth").unwrap_or(0),
                    allows_negative: jbool(b, "allows_negative"),
                    initial_cost_minor: jint(b, "initial_cost_minor")?,
                };
                inventory::create_item(self.conn, ctx, &p, now)
            })(),
            ("POST", "/inventory/receive") => (|| {
                let p = receive_params(b)?;
                inventory::receive_stock(self.conn, ctx, &p, now)
            })(),
            ("POST", "/inventory/issue") => (|| {
                let p = inventory::IssueParams {
                    item_id: jstr(b, "item_id")?,
                    warehouse_id: jstr(b, "warehouse_id")?,
                    qty_kth: jint(b, "qty_kth")?,
                    ref_type: jstr(b, "ref_type")?,
                    ref_id: jstr(b, "ref_id")?,
                };
                inventory::issue_stock(self.conn, ctx, &p, now)
            })(),
            ("POST", "/inventory/adjust") => (|| {
                let item_id = jstr(b, "item_id")?;
                let warehouse_id = jstr(b, "warehouse_id")?;
                let delta_kth = jint(b, "delta_kth")?;
                let reason = jstr(b, "reason")?;
                inventory::adjust_stock(
                    self.conn,
                    ctx,
                    &item_id,
                    &warehouse_id,
                    delta_kth,
                    &reason,
                    now,
                )
            })(),
            ("POST", "/inventory/transfer") => (|| {
                let p = transfer_params(b)?;
                inventory::transfer_stock(self.conn, ctx, &p, now)
            })(),
            ("POST", "/inventory/reserve") => (|| {
                let item_id = jstr(b, "item_id")?;
                let warehouse_id = jstr(b, "warehouse_id")?;
                let qty_kth = jint(b, "qty_kth")?;
                let ref_type = jstr(b, "ref_type")?;
                let ref_id = jstr(b, "ref_id")?;
                inventory::reserve_stock(
                    self.conn,
                    ctx,
                    &item_id,
                    &warehouse_id,
                    qty_kth,
                    &ref_type,
                    &ref_id,
                    now,
                )
            })(),
            ("POST", "/inventory/reservations/status") => (|| {
                let reservation_id = jstr(b, "reservation_id")?;
                let status = jstr(b, "status")?;
                inventory::set_reservation_status(self.conn, ctx, &reservation_id, &status, now)
            })(),
            ("POST", "/inventory/serials") => (|| {
                let item_id = jstr(b, "item_id")?;
                let serial = jstr(b, "serial")?;
                let warehouse_id = jstr(b, "warehouse_id")?;
                inventory::register_serial(self.conn, ctx, &item_id, &serial, &warehouse_id, now)
            })(),
            ("POST", "/inventory/cycle-counts") => (|| {
                let warehouse_id = jstr(b, "warehouse_id")?;
                inventory::create_cycle_count(self.conn, ctx, &warehouse_id, now)
            })(),
            ("POST", "/inventory/cycle-counts/record") => (|| {
                let count_id = jstr(b, "count_id")?;
                let item_id = jstr(b, "item_id")?;
                let counted_qty_kth = jint(b, "counted_qty_kth")?;
                inventory::record_count(self.conn, ctx, &count_id, &item_id, counted_qty_kth, now)
            })(),
            ("POST", "/inventory/cycle-counts/post") => (|| {
                let count_id = jstr(b, "count_id")?;
                let currency = currency_field(b)?;
                inventory::post_cycle_count(self.conn, self.ledger, ctx, &count_id, &currency, now)
            })(),
            ("GET", "/inventory/stock") => {
                let item_id = jopt_str(b, "item_id");
                let warehouse_id = jopt_str(b, "warehouse_id");
                inventory::stock_on_hand_rows(
                    self.conn,
                    &ctx.tenant,
                    item_id.as_deref(),
                    warehouse_id.as_deref(),
                )
                .map(Value::Array)
            }
            ("GET", "/inventory/reorder") => {
                inventory::items_below_reorder(self.conn, &ctx.tenant).map(Value::Array)
            }
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }

    fn sales_route(&self, m: &str, p: &str, b: &Value, ctx: &Ctx, now: i64) -> Option<Res<Value>> {
        let known = matches!(
            p,
            "/sales/customers"
                | "/sales/leads"
                | "/sales/leads/convert"
                | "/sales/opportunities"
                | "/sales/opportunities/stage"
                | "/sales/quotations"
                | "/sales/quotations/status"
                | "/sales/orders"
                | "/sales/orders/from-quotation"
                | "/sales/orders/open"
                | "/sales/orders/status"
                | "/sales/deliveries"
                | "/sales/deliveries/post"
                | "/sales/deliveries/cancel"
                | "/sales/invoices"
                | "/sales/receipts"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/sales/customers") => (|| {
                let p = sales::CreateCustomer {
                    code: jstr(b, "code")?,
                    name: jstr(b, "name")?,
                    currency: currency_field(b)?,
                    credit_limit_minor: jopt_int(b, "credit_limit_minor").unwrap_or(0),
                };
                sales::create_customer(self.conn, ctx, &p, now)
            })(),
            ("POST", "/sales/leads") => (|| {
                let p = sales::CreateLead {
                    name: jstr(b, "name")?,
                    company: jopt_str(b, "company"),
                    contact: jopt_str(b, "contact"),
                    owner: jstr(b, "owner")?,
                };
                sales::create_lead(self.conn, ctx, &p, now)
            })(),
            ("POST", "/sales/leads/convert") => (|| {
                let lead_id = jstr(b, "lead_id")?;
                let title = jstr(b, "title")?;
                sales::convert_lead(self.conn, ctx, &lead_id, &title, now)
            })(),
            ("POST", "/sales/opportunities") => (|| {
                let p = sales::CreateOpportunity {
                    lead_id: jopt_str(b, "lead_id"),
                    customer_name: jstr(b, "customer_name")?,
                    title: jstr(b, "title")?,
                    amount_minor: jint(b, "amount_minor")?,
                    owner: jstr(b, "owner")?,
                };
                sales::create_opportunity(self.conn, ctx, &p, now)
            })(),
            ("POST", "/sales/opportunities/stage") => (|| {
                let opportunity_id = jstr(b, "opportunity_id")?;
                let stage = jstr(b, "stage")?;
                let close_reason = jopt_str(b, "close_reason");
                sales::set_opportunity_stage(
                    self.conn,
                    ctx,
                    &opportunity_id,
                    &stage,
                    close_reason.as_deref(),
                    now,
                )
            })(),
            ("POST", "/sales/quotations") => (|| {
                let p = sales::CreateQuotation {
                    customer_id: jstr(b, "customer_id")?,
                    opportunity_id: jopt_str(b, "opportunity_id"),
                    lines: quote_lines(b)?,
                    discount_minor: jopt_int(b, "discount_minor").unwrap_or(0),
                    tax_bps: jopt_int(b, "tax_bps").unwrap_or(0),
                    expiry_days: jopt_int(b, "expiry_days").unwrap_or(0),
                };
                sales::create_quotation(self.conn, ctx, &p, idem(b).as_deref(), now)
            })(),
            ("POST", "/sales/quotations/status") => (|| {
                let quotation_id = jstr(b, "quotation_id")?;
                let status = jstr(b, "status")?;
                sales::set_quotation_status(self.conn, ctx, &quotation_id, &status, now)
            })(),
            ("POST", "/sales/orders") => (|| {
                let customer_id = jstr(b, "customer_id")?;
                let lines = order_lines(b)?;
                sales::create_sales_order(
                    self.conn,
                    ctx,
                    &customer_id,
                    &lines,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("POST", "/sales/orders/from-quotation") => (|| {
                let quotation_id = jstr(b, "quotation_id")?;
                sales::create_order_from_quotation(
                    self.conn,
                    ctx,
                    &quotation_id,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("GET", "/sales/orders/open") => {
                sales::open_orders(self.conn, &ctx.tenant).map(Value::Array)
            }
            ("GET", "/sales/orders/status") => (|| {
                let order_id = jstr(b, "order_id")?;
                sales::delivered_vs_ordered(self.conn, &ctx.tenant, &order_id).map(Value::Array)
            })(),
            ("POST", "/sales/deliveries") => (|| {
                let order_id = jstr(b, "order_id")?;
                let warehouse_id = jstr(b, "warehouse_id")?;
                let lines = delivery_lines(b)?;
                sales::create_delivery(
                    self.conn,
                    ctx,
                    &order_id,
                    &warehouse_id,
                    &lines,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("POST", "/sales/deliveries/post") => (|| {
                let delivery_id = jstr(b, "delivery_id")?;
                sales::post_delivery(self.conn, ctx, &delivery_id, now)
            })(),
            ("POST", "/sales/deliveries/cancel") => (|| {
                let delivery_id = jstr(b, "delivery_id")?;
                sales::cancel_delivery(self.conn, ctx, &delivery_id, now)
            })(),
            ("POST", "/sales/invoices") => (|| {
                let order_id = jstr(b, "order_id")?;
                sales::create_invoice_from_order(
                    self.conn,
                    ctx,
                    &order_id,
                    self.ledger,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("POST", "/sales/receipts") => (|| {
                let invoice_id = jstr(b, "invoice_id")?;
                let amount_minor = jint(b, "amount_minor")?;
                let method = jstr(b, "method")?;
                sales::record_receipt(
                    self.conn,
                    ctx,
                    &invoice_id,
                    amount_minor,
                    &method,
                    self.ledger,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }

    fn procurement_route(
        &self,
        m: &str,
        p: &str,
        b: &Value,
        ctx: &Ctx,
        now: i64,
    ) -> Option<Res<Value>> {
        let known = matches!(
            p,
            "/procurement/suppliers"
                | "/procurement/requisitions"
                | "/procurement/requisitions/approve"
                | "/procurement/requisitions/reject"
                | "/procurement/rfqs"
                | "/procurement/rfqs/quotes"
                | "/procurement/rfqs/compare"
                | "/procurement/rfqs/award"
                | "/procurement/po"
                | "/procurement/po/from-rfq"
                | "/procurement/po/receive"
                | "/procurement/po/open-lines"
                | "/procurement/bills"
                | "/procurement/bills/approve"
                | "/procurement/payments/proposals"
                | "/procurement/payments/approve"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/procurement/suppliers") => (|| {
                let p = procurement::CreateSupplier {
                    code: jstr(b, "code")?,
                    name: jstr(b, "name")?,
                    currency: currency_field(b)?,
                    payment_terms_days: jopt_int(b, "payment_terms_days").unwrap_or(0),
                };
                procurement::create_supplier(self.conn, ctx, &p, now)
            })(),
            ("POST", "/procurement/requisitions") => (|| {
                let item_id = jstr(b, "item_id")?;
                let qty_kth = jint(b, "qty_kth")?;
                let est_unit_cost_minor = jint(b, "est_unit_cost_minor")?;
                procurement::create_requisition(
                    self.conn,
                    ctx,
                    &item_id,
                    qty_kth,
                    est_unit_cost_minor,
                    now,
                )
            })(),
            ("POST", "/procurement/requisitions/approve") => (|| {
                let requisition_id = jstr(b, "requisition_id")?;
                procurement::approve_requisition(self.conn, ctx, &requisition_id, now)
            })(),
            ("POST", "/procurement/requisitions/reject") => (|| {
                let requisition_id = jstr(b, "requisition_id")?;
                let reason = jstr(b, "reason")?;
                procurement::reject_requisition(self.conn, ctx, &requisition_id, &reason, now)
            })(),
            ("POST", "/procurement/rfqs") => (|| {
                let requisition_id = jstr(b, "requisition_id")?;
                procurement::create_rfq(self.conn, ctx, &requisition_id, now)
            })(),
            ("POST", "/procurement/rfqs/quotes") => (|| {
                let rfq_id = jstr(b, "rfq_id")?;
                let supplier_id = jstr(b, "supplier_id")?;
                let unit_price_minor = jint(b, "unit_price_minor")?;
                let lead_time_days = jint(b, "lead_time_days")?;
                procurement::add_rfq_quote(
                    self.conn,
                    ctx,
                    &rfq_id,
                    &supplier_id,
                    unit_price_minor,
                    lead_time_days,
                    now,
                )
            })(),
            ("GET", "/procurement/rfqs/compare") => (|| {
                let rfq_id = jstr(b, "rfq_id")?;
                procurement::compare_quotes(self.conn, &ctx.tenant, &rfq_id).map(Value::Array)
            })(),
            ("POST", "/procurement/rfqs/award") => (|| {
                let rfq_id = jstr(b, "rfq_id")?;
                let quote_id = jstr(b, "quote_id")?;
                procurement::award_rfq(self.conn, ctx, &rfq_id, &quote_id, now)
            })(),
            ("POST", "/procurement/po") => (|| {
                let supplier_id = jstr(b, "supplier_id")?;
                let lines = po_lines(b)?;
                let expected_date = jopt_str(b, "expected_date");
                procurement::create_po(
                    self.conn,
                    ctx,
                    &supplier_id,
                    &lines,
                    expected_date.as_deref(),
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("POST", "/procurement/po/from-rfq") => (|| {
                let rfq_id = jstr(b, "rfq_id")?;
                procurement::po_from_rfq(self.conn, ctx, &rfq_id, idem(b).as_deref(), now)
            })(),
            ("POST", "/procurement/po/receive") => (|| {
                let po_id = jstr(b, "po_id")?;
                let warehouse_id = jstr(b, "warehouse_id")?;
                let lines = gr_lines(b)?;
                procurement::receive_goods(
                    self.conn,
                    ctx,
                    &po_id,
                    &warehouse_id,
                    &lines,
                    self.ledger,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("GET", "/procurement/po/open-lines") => {
                procurement::open_po_lines(self.conn, &ctx.tenant).map(Value::Array)
            }
            ("POST", "/procurement/bills") => (|| {
                let number = jstr(b, "number")?;
                let supplier_id = jstr(b, "supplier_id")?;
                let po_id = jstr(b, "po_id")?;
                let total_minor = jint(b, "total_minor")?;
                let bill_date = jstr(b, "bill_date")?;
                procurement::create_supplier_bill(
                    self.conn,
                    ctx,
                    &number,
                    &supplier_id,
                    &po_id,
                    total_minor,
                    &bill_date,
                    self.ledger,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("POST", "/procurement/bills/approve") => (|| {
                let bill_id = jstr(b, "bill_id")?;
                procurement::approve_bill_variance(self.conn, ctx, &bill_id, now)
            })(),
            ("POST", "/procurement/payments/proposals") => {
                procurement::create_payment_proposal(self.conn, ctx, now)
            }
            ("POST", "/procurement/payments/approve") => (|| {
                let proposal_id = jstr(b, "proposal_id")?;
                procurement::approve_payment_proposal(
                    self.conn,
                    ctx,
                    &proposal_id,
                    self.ledger,
                    now,
                )
            })(),
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }

    fn manufacturing_route(
        &self,
        m: &str,
        p: &str,
        b: &Value,
        ctx: &Ctx,
        now: i64,
    ) -> Option<Res<Value>> {
        let known = matches!(
            p,
            "/manufacturing/lead-times"
                | "/manufacturing/boms"
                | "/manufacturing/boms/release"
                | "/manufacturing/boms/explode"
                | "/manufacturing/work-orders"
                | "/manufacturing/work-orders/release"
                | "/manufacturing/work-orders/issue"
                | "/manufacturing/work-orders/output"
                | "/manufacturing/work-orders/close"
                | "/manufacturing/mrp/run"
                | "/manufacturing/quality/inspections"
                | "/manufacturing/quality/release"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/manufacturing/lead-times") => (|| {
                let item_id = jstr(b, "item_id")?;
                let days = jint(b, "days")?;
                manufacturing::set_item_lead_time(self.conn, ctx, &item_id, days, now)
            })(),
            ("POST", "/manufacturing/boms") => (|| {
                let item_id = jstr(b, "item_id")?;
                let header_scrap_kth = jopt_int(b, "header_scrap_kth").unwrap_or(0);
                let lines = bom_lines(b)?;
                manufacturing::create_bom(self.conn, ctx, &item_id, header_scrap_kth, &lines, now)
            })(),
            ("POST", "/manufacturing/boms/release") => (|| {
                let bom_id = jstr(b, "bom_id")?;
                manufacturing::release_bom(self.conn, ctx, &bom_id, now)
            })(),
            ("GET", "/manufacturing/boms/explode") => (|| {
                let item_id = jstr(b, "item_id")?;
                let qty_kth = jint(b, "qty_kth")?;
                manufacturing::explode_bom(self.conn, &ctx.tenant, &item_id, qty_kth)
                    .map(Value::Array)
            })(),
            ("POST", "/manufacturing/work-orders") => (|| {
                let item_id = jstr(b, "item_id")?;
                let qty_kth = jint(b, "qty_kth")?;
                let warehouse_id = jstr(b, "warehouse_id")?;
                manufacturing::create_work_order(
                    self.conn,
                    ctx,
                    &item_id,
                    qty_kth,
                    &warehouse_id,
                    now,
                )
            })(),
            ("POST", "/manufacturing/work-orders/release") => (|| {
                let wo_id = jstr(b, "wo_id")?;
                manufacturing::release_work_order(self.conn, ctx, &wo_id, now)
            })(),
            ("POST", "/manufacturing/work-orders/issue") => (|| {
                let wo_id = jstr(b, "wo_id")?;
                let currency = currency_field(b)?;
                manufacturing::issue_wo_materials(
                    self.conn,
                    ctx,
                    &wo_id,
                    self.ledger,
                    &currency,
                    now,
                )
            })(),
            ("POST", "/manufacturing/work-orders/output") => (|| {
                let wo_id = jstr(b, "wo_id")?;
                let qty_kth = jint(b, "qty_kth")?;
                let currency = currency_field(b)?;
                manufacturing::report_output(
                    self.conn,
                    ctx,
                    &wo_id,
                    qty_kth,
                    self.ledger,
                    &currency,
                    now,
                )
            })(),
            ("POST", "/manufacturing/work-orders/close") => (|| {
                let wo_id = jstr(b, "wo_id")?;
                let currency = currency_field(b)?;
                manufacturing::close_work_order(self.conn, ctx, &wo_id, self.ledger, &currency, now)
            })(),
            ("POST", "/manufacturing/mrp/run") => {
                let as_of = jopt_int(b, "as_of").unwrap_or(now);
                manufacturing::mrp_run(self.conn, ctx, as_of)
            }
            ("POST", "/manufacturing/quality/inspections") => (|| {
                let subject_type = jstr(b, "subject_type")?;
                let subject_id = jstr(b, "subject_id")?;
                let item_id = jstr(b, "item_id")?;
                let sample_size_kth = jint(b, "sample_size_kth")?;
                let result = jstr(b, "result")?;
                let hold_qty_kth = jopt_int(b, "hold_qty_kth").unwrap_or(0);
                let warehouse_id = jstr(b, "warehouse_id")?;
                let notes = jopt_str(b, "notes");
                manufacturing::create_inspection(
                    self.conn,
                    ctx,
                    &subject_type,
                    &subject_id,
                    &item_id,
                    sample_size_kth,
                    &result,
                    hold_qty_kth,
                    &warehouse_id,
                    notes.as_deref(),
                    now,
                )
            })(),
            ("POST", "/manufacturing/quality/release") => (|| {
                let inspection_id = jstr(b, "inspection_id")?;
                manufacturing::release_quality_hold(self.conn, ctx, &inspection_id, now)
            })(),
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }

    fn hr_route(&self, m: &str, p: &str, b: &Value, ctx: &Ctx, now: i64) -> Option<Res<Value>> {
        let known = matches!(
            p,
            "/hr/departments"
                | "/hr/positions"
                | "/hr/employees"
                | "/hr/leave-types"
                | "/hr/leave/accrue"
                | "/hr/leave/take"
                | "/hr/attendance"
                | "/hr/payroll/rules"
                | "/hr/payroll/run"
                | "/hr/payroll/summary"
                | "/hr/settlements"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/hr/departments") => (|| {
                let code = jstr(b, "code")?;
                let name = jstr(b, "name")?;
                hr::create_department(self.conn, ctx, &code, &name, now)
            })(),
            ("POST", "/hr/positions") => (|| {
                let code = jstr(b, "code")?;
                let title = jstr(b, "title")?;
                let department_id = jstr(b, "department_id")?;
                hr::create_position(self.conn, ctx, &code, &title, &department_id, now)
            })(),
            ("POST", "/hr/employees") => (|| {
                let code = jstr(b, "code")?;
                let first_name = jstr(b, "first_name")?;
                let last_name = jstr(b, "last_name")?;
                let department_id = jstr(b, "department_id")?;
                let position_id = jopt_str(b, "position_id");
                let hire_date = jstr(b, "hire_date")?;
                let base_salary_minor = jint(b, "base_salary_minor")?;
                let currency = currency_field(b)?;
                hr::create_employee(
                    self.conn,
                    ctx,
                    &code,
                    &first_name,
                    &last_name,
                    &department_id,
                    position_id.as_deref(),
                    &hire_date,
                    base_salary_minor,
                    &currency,
                    now,
                )
            })(),
            ("POST", "/hr/leave-types") => (|| {
                let code = jstr(b, "code")?;
                let name = jstr(b, "name")?;
                let accrual_per_month_kth = jint(b, "accrual_per_month_kth")?;
                hr::create_leave_type(self.conn, ctx, &code, &name, accrual_per_month_kth, now)
            })(),
            ("POST", "/hr/leave/accrue") => (|| {
                let month = jstr(b, "month")?;
                hr::accrue_leave(self.conn, ctx, &month, now)
            })(),
            ("POST", "/hr/leave/take") => (|| {
                let employee_id = jstr(b, "employee_id")?;
                let leave_type_id = jstr(b, "leave_type_id")?;
                let days_kth = jint(b, "days_kth")?;
                let date = jstr(b, "date")?;
                hr::take_leave(
                    self.conn,
                    ctx,
                    &employee_id,
                    &leave_type_id,
                    days_kth,
                    &date,
                    now,
                )
            })(),
            ("POST", "/hr/attendance") => (|| {
                let employee_id = jstr(b, "employee_id")?;
                let date = jstr(b, "date")?;
                let present = jbool(b, "present");
                let hours_kth = jint(b, "hours_kth")?;
                hr::mark_attendance(self.conn, ctx, &employee_id, &date, present, hours_kth, now)
            })(),
            ("POST", "/hr/payroll/rules") => (|| {
                let p = hr::CreatePayrollRule {
                    code: jstr(b, "code")?,
                    name: jstr(b, "name")?,
                    kind: jstr(b, "kind")?,
                    rate_kth: jopt_int(b, "rate_kth"),
                    amount_minor: jopt_int(b, "amount_minor"),
                    employer_side: jbool(b, "employer_side"),
                    order_idx: jopt_int(b, "order_idx").unwrap_or(0),
                };
                hr::create_payroll_rule(self.conn, ctx, &p, now)
            })(),
            ("POST", "/hr/payroll/run") => (|| {
                let period = jstr(b, "period")?;
                hr::run_payroll(
                    self.conn,
                    ctx,
                    &period,
                    self.ledger,
                    idem(b).as_deref(),
                    now,
                )
            })(),
            ("GET", "/hr/payroll/summary") => (|| {
                let period = jstr(b, "period")?;
                hr::payroll_summary(self.conn, &ctx.tenant, &period)
            })(),
            ("POST", "/hr/settlements") => (|| {
                let employee_id = jstr(b, "employee_id")?;
                let settlement_date = jstr(b, "settlement_date")?;
                let notice_recovery_minor = jint(b, "notice_recovery_minor")?;
                let currency = currency_field(b)?;
                hr::final_settlement(
                    self.conn,
                    ctx,
                    &employee_id,
                    &settlement_date,
                    notice_recovery_minor,
                    self.ledger,
                    &currency,
                    now,
                )
            })(),
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }

    fn projects_route(
        &self,
        m: &str,
        p: &str,
        b: &Value,
        ctx: &Ctx,
        now: i64,
    ) -> Option<Res<Value>> {
        let known = matches!(
            p,
            "/projects"
                | "/projects/tasks"
                | "/projects/time"
                | "/projects/time/approve"
                | "/projects/invoice"
                | "/projects/milestones"
                | "/projects/milestones/achieve"
                | "/projects/milestones/invoice"
                | "/projects/status"
                | "/service/sla-policies"
                | "/service/calendar"
                | "/service/tickets"
                | "/service/tickets/pause"
                | "/service/tickets/resume"
                | "/service/tickets/check-sla"
                | "/service/tickets/resolve"
                | "/service/tickets/knowledge"
                | "/service/tickets/csat"
        );
        if !known {
            return None;
        }
        let res = match (m, p) {
            ("POST", "/projects") => (|| {
                let code = jstr(b, "code")?;
                let name = jstr(b, "name")?;
                let customer_id = jopt_str(b, "customer_id");
                let billable = jbool(b, "billable");
                let rate_minor_per_hour = jopt_int(b, "rate_minor_per_hour").unwrap_or(0);
                let budget_hours_kth = jopt_int(b, "budget_hours_kth").unwrap_or(0);
                projects_service::create_project(
                    self.conn,
                    ctx,
                    &code,
                    &name,
                    customer_id.as_deref(),
                    billable,
                    rate_minor_per_hour,
                    budget_hours_kth,
                    now,
                )
            })(),
            ("POST", "/projects/tasks") => (|| {
                let project_id = jstr(b, "project_id")?;
                let title = jstr(b, "title")?;
                projects_service::create_task(self.conn, ctx, &project_id, &title, now)
            })(),
            ("POST", "/projects/time") => (|| {
                let project_id = jstr(b, "project_id")?;
                let task_id = jopt_str(b, "task_id");
                let employee_id = jstr(b, "employee_id")?;
                let date = jstr(b, "date")?;
                let hours_kth = jint(b, "hours_kth")?;
                let billable = jbool(b, "billable");
                projects_service::log_time(
                    self.conn,
                    ctx,
                    &project_id,
                    task_id.as_deref(),
                    &employee_id,
                    &date,
                    hours_kth,
                    billable,
                    now,
                )
            })(),
            ("POST", "/projects/time/approve") => (|| {
                let timesheet_id = jstr(b, "timesheet_id")?;
                projects_service::approve_timesheet(self.conn, ctx, &timesheet_id, now)
            })(),
            ("POST", "/projects/invoice") => (|| {
                let project_id = jstr(b, "project_id")?;
                projects_service::invoice_timesheets(self.conn, ctx, &project_id, self.ledger, now)
            })(),
            ("POST", "/projects/milestones") => (|| {
                let project_id = jstr(b, "project_id")?;
                let name = jstr(b, "name")?;
                let amount_minor = jint(b, "amount_minor")?;
                projects_service::add_milestone(
                    self.conn,
                    ctx,
                    &project_id,
                    &name,
                    amount_minor,
                    now,
                )
            })(),
            ("POST", "/projects/milestones/achieve") => (|| {
                let milestone_id = jstr(b, "milestone_id")?;
                projects_service::achieve_milestone(self.conn, ctx, &milestone_id, now)
            })(),
            ("POST", "/projects/milestones/invoice") => (|| {
                let milestone_id = jstr(b, "milestone_id")?;
                projects_service::invoice_milestone(self.conn, ctx, &milestone_id, self.ledger, now)
            })(),
            ("GET", "/projects/status") => (|| {
                let project_id = jstr(b, "project_id")?;
                projects_service::project_status(self.conn, &ctx.tenant, &project_id)
            })(),
            ("POST", "/service/sla-policies") => (|| {
                let name = jstr(b, "name")?;
                let response_minutes = jint(b, "response_minutes")?;
                let resolve_minutes = jint(b, "resolve_minutes")?;
                let escalation_minutes = jint(b, "escalation_minutes")?;
                projects_service::create_sla_policy(
                    self.conn,
                    ctx,
                    &name,
                    response_minutes,
                    resolve_minutes,
                    escalation_minutes,
                    now,
                )
            })(),
            ("POST", "/service/calendar") => (|| {
                let date = jstr(b, "date")?;
                let working = jbool(b, "working");
                projects_service::set_calendar_day(self.conn, ctx, &date, working, now)
            })(),
            ("POST", "/service/tickets") => (|| {
                let subject = jstr(b, "subject")?;
                let customer_id = jopt_str(b, "customer_id");
                let priority = jstr(b, "priority")?;
                let sla_policy_id = jstr(b, "sla_policy_id")?;
                projects_service::create_ticket(
                    self.conn,
                    ctx,
                    &subject,
                    customer_id.as_deref(),
                    &priority,
                    &sla_policy_id,
                    now,
                )
            })(),
            ("POST", "/service/tickets/pause") => (|| {
                let ticket_id = jstr(b, "ticket_id")?;
                projects_service::pause_ticket(self.conn, ctx, &ticket_id, now)
            })(),
            ("POST", "/service/tickets/resume") => (|| {
                let ticket_id = jstr(b, "ticket_id")?;
                projects_service::resume_ticket(self.conn, ctx, &ticket_id, now)
            })(),
            ("POST", "/service/tickets/check-sla") => (|| {
                let ticket_id = jstr(b, "ticket_id")?;
                projects_service::check_sla(self.conn, ctx, &ticket_id, now)
            })(),
            ("POST", "/service/tickets/resolve") => (|| {
                let ticket_id = jstr(b, "ticket_id")?;
                projects_service::resolve_ticket(self.conn, ctx, &ticket_id, now)
            })(),
            ("POST", "/service/tickets/knowledge") => (|| {
                let ticket_id = jstr(b, "ticket_id")?;
                let reference = jstr(b, "reference")?;
                projects_service::link_knowledge(self.conn, ctx, &ticket_id, &reference, now)
            })(),
            ("POST", "/service/tickets/csat") => (|| {
                let ticket_id = jstr(b, "ticket_id")?;
                let score = jint(b, "score")?;
                projects_service::record_csat(self.conn, ctx, &ticket_id, score, now)
            })(),
            _ => Err(format!("method not allowed: {} {}", m, p)),
        };
        Some(res)
    }
}

pub fn status_for(error: &str) -> u16 {
    if error.contains("permission denied") {
        403
    } else if error.contains("not found") {
        404
    } else if error.contains("already")
        || error.contains("idempotency key")
        || error.contains("duplicate")
    {
        409
    } else if error.contains("method not allowed") {
        405
    } else {
        400
    }
}

pub fn parse_query(query: &str) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = pct_decode(k);
        let value = pct_decode(v);
        let json_val = match query_number(&value) {
            Some(n) => Value::from(n),
            None => Value::String(value),
        };
        map.insert(key, json_val);
    }
    map
}

fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 3 <= bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn query_number(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

fn idem(b: &Value) -> Option<String> {
    jopt_str(b, "idempotency_key")
}

fn currency_field(b: &Value) -> Res<String> {
    let c = jstr(b, "currency")?;
    if c.len() != 3 || !c.bytes().all(|x| x.is_ascii_alphabetic()) {
        return Err("currency: must be exactly 3 letters".to_string());
    }
    Ok(c)
}

fn receive_params(b: &Value) -> Res<inventory::ReceiveParams> {
    Ok(inventory::ReceiveParams {
        item_id: jstr(b, "item_id")?,
        warehouse_id: jstr(b, "warehouse_id")?,
        qty_kth: jint(b, "qty_kth")?,
        unit_cost_minor: jint(b, "unit_cost_minor")?,
        batch_no: jopt_str(b, "batch_no"),
        batch_expiry: jopt_str(b, "batch_expiry"),
        ref_type: jopt_str(b, "ref_type"),
        ref_id: jopt_str(b, "ref_id"),
    })
}

fn transfer_params(b: &Value) -> Res<inventory::TransferParams> {
    Ok(inventory::TransferParams {
        item_id: jstr(b, "item_id")?,
        from_wh: jstr(b, "from_wh")?,
        to_wh: jstr(b, "to_wh")?,
        qty_kth: jint(b, "qty_kth")?,
    })
}

fn line_error(i: usize, err: String) -> String {
    format!("lines[{}]: {}", i, err)
}

fn quote_lines(b: &Value) -> Res<Vec<sales::QuoteLineIn>> {
    let arr = jarr(b, "lines")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, l) in arr.iter().enumerate() {
        let line = (|| -> Res<sales::QuoteLineIn> {
            Ok(sales::QuoteLineIn {
                item_id: jopt_str(l, "item_id"),
                description: jstr(l, "description")?,
                qty_kth: jint(l, "qty_kth")?,
                unit_price_minor: jint(l, "unit_price_minor")?,
                discount_pct_kth: jopt_int(l, "discount_pct_kth").unwrap_or(0),
                tax_bps: jopt_int(l, "tax_bps").unwrap_or(0),
            })
        })()
        .map_err(|err| line_error(i, err))?;
        out.push(line);
    }
    Ok(out)
}

fn order_lines(b: &Value) -> Res<Vec<sales::OrderLineIn>> {
    let arr = jarr(b, "lines")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, l) in arr.iter().enumerate() {
        let line = (|| -> Res<sales::OrderLineIn> {
            Ok(sales::OrderLineIn {
                item_id: jstr(l, "item_id")?,
                qty_kth: jint(l, "qty_kth")?,
                unit_price_minor: jint(l, "unit_price_minor")?,
                tax_bps: jopt_int(l, "tax_bps").unwrap_or(0),
            })
        })()
        .map_err(|err| line_error(i, err))?;
        out.push(line);
    }
    Ok(out)
}

fn delivery_lines(b: &Value) -> Res<Vec<sales::DeliveryLineIn>> {
    let arr = jarr(b, "lines")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, l) in arr.iter().enumerate() {
        let line = (|| -> Res<sales::DeliveryLineIn> {
            Ok(sales::DeliveryLineIn {
                order_line_no: jint(l, "order_line_no")?,
                qty_kth: jint(l, "qty_kth")?,
            })
        })()
        .map_err(|err| line_error(i, err))?;
        out.push(line);
    }
    Ok(out)
}

fn po_lines(b: &Value) -> Res<Vec<procurement::PoLineIn>> {
    let arr = jarr(b, "lines")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, l) in arr.iter().enumerate() {
        let line = (|| -> Res<procurement::PoLineIn> {
            Ok(procurement::PoLineIn {
                item_id: jstr(l, "item_id")?,
                qty_kth: jint(l, "qty_kth")?,
                unit_cost_minor: jint(l, "unit_cost_minor")?,
            })
        })()
        .map_err(|err| line_error(i, err))?;
        out.push(line);
    }
    Ok(out)
}

fn gr_lines(b: &Value) -> Res<Vec<procurement::GrLineIn>> {
    let arr = jarr(b, "lines")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, l) in arr.iter().enumerate() {
        let line = (|| -> Res<procurement::GrLineIn> {
            Ok(procurement::GrLineIn {
                po_line_no: jint(l, "po_line_no")?,
                qty_kth: jint(l, "qty_kth")?,
                batch_no: jopt_str(l, "batch_no"),
                batch_expiry: jopt_str(l, "batch_expiry"),
            })
        })()
        .map_err(|err| line_error(i, err))?;
        out.push(line);
    }
    Ok(out)
}

fn bom_lines(b: &Value) -> Res<Vec<manufacturing::BomLineIn>> {
    let arr = jarr(b, "lines")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, l) in arr.iter().enumerate() {
        let line = (|| -> Res<manufacturing::BomLineIn> {
            Ok(manufacturing::BomLineIn {
                component_id: jstr(l, "component_id")?,
                qty_kth: jint(l, "qty_kth")?,
                scrap_kth: jopt_int(l, "scrap_kth").unwrap_or(0),
            })
        })()
        .map_err(|err| line_error(i, err))?;
        out.push(line);
    }
    Ok(out)
}
