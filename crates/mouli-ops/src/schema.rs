//! All table definitions (SQLite). Every table carries `tenant_id` and every foreign key is tenant-scoped.

use crate::common::{e, Res};
use rusqlite::Connection;

/// Create every table and index if it does not exist.
pub fn create_schema(conn: &Connection) -> Res<()> {
    conn.execute_batch(r#"
CREATE TABLE IF NOT EXISTS role_permissions (
  tenant_id TEXT NOT NULL,
  role TEXT NOT NULL,
  permission TEXT NOT NULL,
  PRIMARY KEY (tenant_id, role, permission)
);
CREATE TABLE IF NOT EXISTS approval_limits (
  tenant_id TEXT NOT NULL,
  role TEXT NOT NULL,
  limit_minor INTEGER NOT NULL,           -- -1 means unlimited
  PRIMARY KEY (tenant_id, role)
);
CREATE TABLE IF NOT EXISTS numbering_series (
  tenant_id TEXT NOT NULL,
  company TEXT NOT NULL,
  series TEXT NOT NULL,
  year INTEGER NOT NULL,
  next_seq INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (tenant_id, company, series, year)
);
CREATE TABLE IF NOT EXISTS idempotency (
  tenant_id TEXT NOT NULL,
  key TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  result_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, key)
);
CREATE TABLE IF NOT EXISTS audit_log (
  tenant_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  payload TEXT NOT NULL,
  previous_hash TEXT NOT NULL,
  entry_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, seq)
);
CREATE TABLE IF NOT EXISTS attachments (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  name TEXT NOT NULL,
  hash TEXT NOT NULL,
  size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id)
);
CREATE TABLE IF NOT EXISTS notifications (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  user_name TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  read_flag INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id)
);

-- ---------- inventory ----------
CREATE TABLE IF NOT EXISTS items (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  uom TEXT NOT NULL,
  valuation_method TEXT NOT NULL CHECK (valuation_method IN ('FIFO','WA')),
  weighted_avg_cost_minor INTEGER NOT NULL DEFAULT 0,
  reorder_point_kth INTEGER NOT NULL DEFAULT 0,
  safety_stock_kth INTEGER NOT NULL DEFAULT 0,
  lead_time_days INTEGER NOT NULL DEFAULT 0,
  allows_negative INTEGER NOT NULL DEFAULT 0,
  active INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS uom_conversions (
  tenant_id TEXT NOT NULL,
  from_uom TEXT NOT NULL,
  to_uom TEXT NOT NULL,
  factor_kth INTEGER NOT NULL CHECK (factor_kth > 0),
  PRIMARY KEY (tenant_id, from_uom, to_uom)
);
CREATE TABLE IF NOT EXISTS warehouses (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS bins (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  code TEXT NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, warehouse_id, code),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
-- Append-only movement ledger. qty_kth is signed: positive in, negative out.
CREATE TABLE IF NOT EXISTS stock_movements (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  movement_seq INTEGER NOT NULL,
  item_id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  bin_id TEXT,
  batch_no TEXT,
  movement_type TEXT NOT NULL CHECK (movement_type IN ('receipt','issue','transfer_in','transfer_out','adjustment','return')),
  qty_kth INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL DEFAULT 0,
  ref_type TEXT,
  ref_id TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, movement_seq),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE INDEX IF NOT EXISTS idx_movements_item ON stock_movements(tenant_id, item_id, movement_seq);
-- FIFO remainder queue (per receipt layer).
CREATE TABLE IF NOT EXISTS fifo_lots (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  batch_no TEXT,
  qty_remaining_kth INTEGER NOT NULL CHECK (qty_remaining_kth >= 0),
  unit_cost_minor INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS batches (
  tenant_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  batch_no TEXT NOT NULL,
  expiry_yyyymmdd TEXT NOT NULL,
  PRIMARY KEY (tenant_id, item_id, batch_no),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS serial_numbers (
  tenant_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  serial TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('in_stock','shipped','scrapped')),
  warehouse_id TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, item_id, serial),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS reservations (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  ref_type TEXT NOT NULL,
  ref_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('open','released','fulfilled')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS cycle_counts (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('open','posted')),
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  posted_at INTEGER,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS cycle_count_lines (
  tenant_id TEXT NOT NULL,
  count_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  bin_id TEXT,
  system_qty_kth INTEGER NOT NULL,
  counted_qty_kth INTEGER,
  PRIMARY KEY (tenant_id, count_id, item_id),
  FOREIGN KEY (tenant_id, count_id) REFERENCES cycle_counts(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);

-- ---------- sales / CRM ----------
CREATE TABLE IF NOT EXISTS customers (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  currency TEXT NOT NULL,
  credit_limit_minor INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS leads (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  name TEXT NOT NULL,
  company TEXT,
  contact TEXT,
  status TEXT NOT NULL CHECK (status IN ('new','qualified','converted','disqualified')),
  owner TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id)
);
CREATE TABLE IF NOT EXISTS opportunities (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  lead_id TEXT,
  customer_name TEXT NOT NULL,
  title TEXT NOT NULL,
  amount_minor INTEGER NOT NULL DEFAULT 0,
  stage TEXT NOT NULL CHECK (stage IN ('prospecting','proposal','negotiation','won','lost')),
  close_reason TEXT,
  owner TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, lead_id) REFERENCES leads(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS quotations (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  customer_id TEXT NOT NULL,
  opportunity_id TEXT,
  version INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL CHECK (status IN ('draft','sent','accepted','rejected','expired')),
  currency TEXT NOT NULL,
  discount_minor INTEGER NOT NULL DEFAULT 0,
  tax_bps INTEGER NOT NULL DEFAULT 0,
  expiry_at INTEGER NOT NULL,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, id),
  FOREIGN KEY (tenant_id, opportunity_id) REFERENCES opportunities(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS quotation_lines (
  tenant_id TEXT NOT NULL,
  quotation_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  item_id TEXT,
  description TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  unit_price_minor INTEGER NOT NULL CHECK (unit_price_minor >= 0),
  discount_pct_kth INTEGER NOT NULL DEFAULT 0,
  tax_bps INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, quotation_id, line_no),
  FOREIGN KEY (tenant_id, quotation_id) REFERENCES quotations(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS sales_orders (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  customer_id TEXT NOT NULL,
  quotation_id TEXT,
  status TEXT NOT NULL CHECK (status IN ('open','partially_delivered','delivered','cancelled')),
  currency TEXT NOT NULL,
  total_minor INTEGER NOT NULL DEFAULT 0,
  delivery_date TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, id),
  FOREIGN KEY (tenant_id, quotation_id) REFERENCES quotations(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS sales_order_lines (
  tenant_id TEXT NOT NULL,
  order_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  qty_delivered_kth INTEGER NOT NULL DEFAULT 0,
  unit_price_minor INTEGER NOT NULL,
  tax_bps INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, order_id, line_no),
  FOREIGN KEY (tenant_id, order_id) REFERENCES sales_orders(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS deliveries (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  order_id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('draft','posted','cancelled')),
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  posted_at INTEGER,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, order_id) REFERENCES sales_orders(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS delivery_lines (
  tenant_id TEXT NOT NULL,
  delivery_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  order_line_no INTEGER NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  PRIMARY KEY (tenant_id, delivery_id, line_no),
  FOREIGN KEY (tenant_id, delivery_id) REFERENCES deliveries(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS invoices (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  customer_id TEXT NOT NULL,
  order_id TEXT,
  currency TEXT NOT NULL,
  total_minor INTEGER NOT NULL DEFAULT 0,
  tax_minor INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('posted','partially_paid','paid','void')),
  due_date TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, id),
  FOREIGN KEY (tenant_id, order_id) REFERENCES sales_orders(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS invoice_lines (
  tenant_id TEXT NOT NULL,
  invoice_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  item_id TEXT,
  description TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  unit_price_minor INTEGER NOT NULL,
  total_minor INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, invoice_id, line_no),
  FOREIGN KEY (tenant_id, invoice_id) REFERENCES invoices(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS receipts (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  invoice_id TEXT NOT NULL,
  amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
  method TEXT NOT NULL,
  received_at INTEGER NOT NULL,
  created_by TEXT NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, invoice_id) REFERENCES invoices(tenant_id, id)

);

-- ---------- procurement ----------
CREATE TABLE IF NOT EXISTS suppliers (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  currency TEXT NOT NULL,
  payment_terms_days INTEGER NOT NULL DEFAULT 30,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS requisitions (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  est_unit_cost_minor INTEGER NOT NULL DEFAULT 0,
  amount_minor INTEGER NOT NULL DEFAULT 0,
  requester TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending','approved','escalated','rejected','converted')),
  approver TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS rfqs (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  requisition_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('open','awarded','closed')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, requisition_id) REFERENCES requisitions(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS rfq_quotes (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  rfq_id TEXT NOT NULL,
  supplier_id TEXT NOT NULL,
  unit_price_minor INTEGER NOT NULL,
  lead_time_days INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('received','awarded','rejected')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, rfq_id) REFERENCES rfqs(tenant_id, id),
  FOREIGN KEY (tenant_id, supplier_id) REFERENCES suppliers(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS purchase_orders (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  supplier_id TEXT NOT NULL,
  rfq_id TEXT,
  status TEXT NOT NULL CHECK (status IN ('open','partially_received','received','closed','cancelled')),
  currency TEXT NOT NULL,
  total_minor INTEGER NOT NULL DEFAULT 0,
  expected_date TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, supplier_id) REFERENCES suppliers(tenant_id, id),
  FOREIGN KEY (tenant_id, rfq_id) REFERENCES rfqs(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS purchase_order_lines (
  tenant_id TEXT NOT NULL,
  po_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  qty_received_kth INTEGER NOT NULL DEFAULT 0,
  unit_cost_minor INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, po_id, line_no),
  FOREIGN KEY (tenant_id, po_id) REFERENCES purchase_orders(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS goods_receipts (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  po_id TEXT NOT NULL,
  warehouse_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('posted')),
  received_at INTEGER NOT NULL,
  created_by TEXT NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, po_id) REFERENCES purchase_orders(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS goods_receipt_lines (
  tenant_id TEXT NOT NULL,
  gr_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  po_line_no INTEGER NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  batch_no TEXT,
  PRIMARY KEY (tenant_id, gr_id, line_no),
  FOREIGN KEY (tenant_id, gr_id) REFERENCES goods_receipts(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS supplier_bills (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  supplier_id TEXT NOT NULL,
  po_id TEXT NOT NULL,
  total_minor INTEGER NOT NULL,
  price_variance_minor INTEGER NOT NULL DEFAULT 0,
  match_status TEXT NOT NULL CHECK (match_status IN ('pending','matched','flagged','approved')),
  status TEXT NOT NULL CHECK (status IN ('posted','paid')),
  bill_date TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, supplier_id) REFERENCES suppliers(tenant_id, id),
  FOREIGN KEY (tenant_id, po_id) REFERENCES purchase_orders(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS payment_proposals (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('draft','approved','paid')),
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  approved_by TEXT,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number)
);
CREATE TABLE IF NOT EXISTS payment_proposal_lines (
  tenant_id TEXT NOT NULL,
  proposal_id TEXT NOT NULL,
  bill_id TEXT NOT NULL,
  amount_minor INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, proposal_id, bill_id),
  UNIQUE (tenant_id, bill_id),
  FOREIGN KEY (tenant_id, proposal_id) REFERENCES payment_proposals(tenant_id, id),
  FOREIGN KEY (tenant_id, bill_id) REFERENCES supplier_bills(tenant_id, id)

);

-- ---------- manufacturing ----------
CREATE TABLE IF NOT EXISTS boms (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  scrap_kth INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('draft','released','obsolete')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, item_id, version),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS bom_lines (
  tenant_id TEXT NOT NULL,
  bom_id TEXT NOT NULL,
  line_no INTEGER NOT NULL,
  component_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  scrap_kth INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, bom_id, line_no),
  FOREIGN KEY (tenant_id, bom_id) REFERENCES boms(tenant_id, id),
  FOREIGN KEY (tenant_id, component_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS work_orders (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  item_id TEXT NOT NULL,
  bom_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  qty_output_kth INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('created','released','in_progress','closed')),
  warehouse_id TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  closed_at INTEGER,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id),
  FOREIGN KEY (tenant_id, bom_id) REFERENCES boms(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS work_order_issues (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  wo_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  unit_cost_minor INTEGER NOT NULL DEFAULT 0,
  warehouse_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, wo_id) REFERENCES work_orders(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id),
  FOREIGN KEY (tenant_id, warehouse_id) REFERENCES warehouses(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS work_order_outputs (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  wo_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  qty_kth INTEGER NOT NULL CHECK (qty_kth > 0),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, wo_id) REFERENCES work_orders(tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS inspections (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  subject_type TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  sample_size_kth INTEGER NOT NULL DEFAULT 1000,
  result TEXT NOT NULL CHECK (result IN ('pass','fail','hold')),
  notes TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items(tenant_id, id)

);

-- ---------- HR and payroll ----------
CREATE TABLE IF NOT EXISTS departments (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS positions (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  title TEXT NOT NULL,
  department_id TEXT NOT NULL,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code),
  FOREIGN KEY (tenant_id, department_id) REFERENCES departments(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS employees (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  first_name TEXT NOT NULL,
  last_name TEXT NOT NULL,
  department_id TEXT NOT NULL,
  position_id TEXT,
  hire_date TEXT NOT NULL,
  base_salary_minor INTEGER NOT NULL DEFAULT 0,
  currency TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','terminated')),
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code),
  FOREIGN KEY (tenant_id, department_id) REFERENCES departments(tenant_id, id),
  FOREIGN KEY (tenant_id, position_id) REFERENCES positions(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS leave_types (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  accrual_per_month_kth INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS leave_balances (
  tenant_id TEXT NOT NULL,
  employee_id TEXT NOT NULL,
  leave_type_id TEXT NOT NULL,
  balance_kth INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, employee_id, leave_type_id),
  FOREIGN KEY (tenant_id, employee_id) REFERENCES employees(tenant_id, id),
  FOREIGN KEY (tenant_id, leave_type_id) REFERENCES leave_types(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS attendance (
  tenant_id TEXT NOT NULL,
  employee_id TEXT NOT NULL,
  date TEXT NOT NULL,
  present INTEGER NOT NULL DEFAULT 1,
  hours_kth INTEGER NOT NULL DEFAULT 8000,
  PRIMARY KEY (tenant_id, employee_id, date),
  FOREIGN KEY (tenant_id, employee_id) REFERENCES employees(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS payroll_rules (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('percent_of_gross','percent_of_basic','fixed_minor')),
  rate_kth INTEGER,
  amount_minor INTEGER,
  employer_side INTEGER NOT NULL DEFAULT 0,
  order_idx INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code)
);
CREATE TABLE IF NOT EXISTS payroll_runs (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  period TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('draft','posted')),
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  posted_at INTEGER,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number)
);
CREATE TABLE IF NOT EXISTS payslips (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  employee_id TEXT NOT NULL,
  gross_minor INTEGER NOT NULL,
  deductions_minor INTEGER NOT NULL,
  employer_cost_minor INTEGER NOT NULL,
  net_minor INTEGER NOT NULL,
  proration_kth INTEGER NOT NULL DEFAULT 1000,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, run_id, employee_id),
  FOREIGN KEY (tenant_id, run_id) REFERENCES payroll_runs(tenant_id, id),
  FOREIGN KEY (tenant_id, employee_id) REFERENCES employees(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS payslip_lines (
  tenant_id TEXT NOT NULL,
  payslip_id TEXT NOT NULL,
  rule_code TEXT NOT NULL,
  side TEXT NOT NULL CHECK (side IN ('deduction','employer')),
  amount_minor INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, payslip_id, rule_code, side),
  FOREIGN KEY (tenant_id, payslip_id) REFERENCES payslips(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS settlements (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  employee_id TEXT NOT NULL,
  settlement_date TEXT NOT NULL,
  leave_payout_minor INTEGER NOT NULL DEFAULT 0,
  gratuity_minor INTEGER NOT NULL DEFAULT 0,
  notice_recovery_minor INTEGER NOT NULL DEFAULT 0,
  net_minor INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, employee_id) REFERENCES employees(tenant_id, id)

);

-- ---------- projects and service ----------
CREATE TABLE IF NOT EXISTS projects (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  customer_id TEXT,
  billable INTEGER NOT NULL DEFAULT 1,
  rate_minor_per_hour INTEGER NOT NULL DEFAULT 0,
  budget_hours_kth INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('active','closed')),
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, code),
  FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS tasks (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('open','done')),
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES projects(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS timesheets (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  task_id TEXT,
  employee_id TEXT NOT NULL,
  date TEXT NOT NULL,
  hours_kth INTEGER NOT NULL CHECK (hours_kth > 0),
  billable INTEGER NOT NULL DEFAULT 1,
  rate_minor_per_hour INTEGER NOT NULL DEFAULT 0,
  approved INTEGER NOT NULL DEFAULT 0,
  invoiced INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES projects(tenant_id, id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES tasks(tenant_id, id),
  FOREIGN KEY (tenant_id, employee_id) REFERENCES employees(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS milestones (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  name TEXT NOT NULL,
  amount_minor INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending','achieved','invoiced')),
  invoice_id TEXT,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES projects(tenant_id, id),
  FOREIGN KEY (tenant_id, invoice_id) REFERENCES invoices(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS sla_policies (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  name TEXT NOT NULL,
  response_minutes INTEGER NOT NULL DEFAULT 60,
  resolve_minutes INTEGER NOT NULL DEFAULT 480,
  escalation_minutes INTEGER NOT NULL DEFAULT 240,
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, name)
);
-- Working-day calendar; if a date has no row, Mon-Fri are working days, Sat/Sun are not.
CREATE TABLE IF NOT EXISTS calendar_days (
  tenant_id TEXT NOT NULL,
  date TEXT NOT NULL,
  working INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, date)
);
CREATE TABLE IF NOT EXISTS tickets (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  number TEXT NOT NULL,
  subject TEXT NOT NULL,
  customer_id TEXT,
  priority TEXT NOT NULL CHECK (priority IN ('low','normal','high','urgent')),
  status TEXT NOT NULL CHECK (status IN ('open','paused','resolved','closed')),
  sla_policy_id TEXT NOT NULL,
  assignee TEXT,
  created_at INTEGER NOT NULL,
  resolved_at INTEGER,
  sla_accum_minutes INTEGER NOT NULL DEFAULT 0,
  clock_started_at INTEGER NOT NULL,
  paused_flag INTEGER NOT NULL DEFAULT 0,
  breach_notified INTEGER NOT NULL DEFAULT 0,
  escalated INTEGER NOT NULL DEFAULT 0,
  csat_score INTEGER CHECK (csat_score IS NULL OR (csat_score >= 1 AND csat_score <= 5)),
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, number),
  FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, id),
  FOREIGN KEY (tenant_id, sla_policy_id) REFERENCES sla_policies(tenant_id, id)

);
CREATE TABLE IF NOT EXISTS ticket_events (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  ticket_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('comment','pause','resume','escalate','resolve','knowledge_link','csat','breach')),
  detail TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, ticket_id) REFERENCES tickets(tenant_id, id)

);
"#).map_err(e)?;
    Ok(())
}
