# MOULI-BUSINESS — Unified Agentic Business Operating System

## Product mandate

Build one integrated AI-native business operating system by combining the supplied MOULI-CRM and MOULI-ERP specifications. The product name in this repository is **MOULI-BUSINESS**. It provides one coherent experience and shared business domain while preserving clear ownership boundaries.

This is not a second enterprise authority. It is a business application layer that consumes MOULI platform authorities.

## Authority boundaries

| Platform | Sole responsibility |
|---|---|
| MOULI | Core runtime and compute/model execution |
| MOULIONE | Identity, authentication, authorization, tenancy, company/legal-entity/plant identity, enterprise policy, workflow/approval and audit authority |
| MOULI-DATA | Common persistence authority: relational, document, graph, vector, object, search and event capabilities |
| MOULI-RAG | Grounded retrieval and enterprise/customer knowledge intelligence through MOULI-DATA |
| AgentBox | Agent lifecycle, execution and orchestration where available |
| MOULI-BUSINESS | Unified business application and cross-domain business process coordination |

MOULI-BUSINESS contains CRM and ERP domain capabilities. It must not create a competing identity provider, tenant registry, workflow authority, audit authority, RAG platform, agent platform or independent database.

## Unified business domains

### Customer and commercial lifecycle (CRM)
- Organizations, business groups, accounts, contacts, stakeholders and relationship graphs.
- Lead capture, qualification, scoring, assignment, nurturing, conversion and attribution.
- Opportunities, configurable pipelines, forecasting, deal risk and sales activities.
- Sales teams, territories, targets, quotations, products, services, price books and commissions.
- Campaigns, segmentation, consent, channel attribution and marketing automation.
- Customer service, tickets, SLA, knowledge base, customer success, renewals, retention and churn risk.
- Customer 360, unified timeline, authorized communications and customer intelligence.

### Enterprise operations (ERP)
- Enterprise master data references and organizational structures.
- General ledger, AP, AR, cash, treasury, fixed assets, controlling, costing, budgets, EPM and close.
- Supplier lifecycle, sourcing, requisitions, RFQs, purchase orders, receipts, invoice matching and spend analysis.
- Inventory, batches, serials, valuation, warehouses, picking, packing, shipping and stock reconciliation.
- Demand/supply planning, logistics, transportation and fulfillment.
- BOM, routings, MRP, capacity, production orders, shop-floor execution, quality, CAPA and maintenance.
- Sales orders, delivery, billing, receivables and revenue processes.
- Workforce, HR, attendance, leave, payroll integration and workforce analytics.
- Projects, WBS, portfolio, resource planning, project costing and billing.
- Tax and statutory integration through native MOULIONE/MOULI-DATA capabilities, including India GST/TDS/TCS support.
- Industry packs: manufacturing, NBFC/digital lending, microfinance, schools, kirana/retail, construction, agriculture, logistics and professional services.

### Cross-domain business processes
- Lead-to-opportunity-to-order-to-cash.
- Campaign-to-lead-to-customer.
- Requisition-to-purchase-to-pay.
- Plan-to-produce-to-stock.
- Order-to-delivery-to-invoice-to-collection.
- Hire-to-retire (integrating authorized HR/payroll capabilities).
- Project-to-budget-to-delivery-to-billing.
- Loan application-to-underwriting-to-authorized-disbursement-to-collection.
- Customer issue-to-ticket-to-resolution-to-success follow-up.

A shared canonical entity reference must prevent duplicate customer, supplier, product, company, legal-entity, plant and employee authorities. CRM owns relationship processes; ERP owns operational and financial transactions. Shared identifiers and cross-domain references are resolved through MOULIONE and MOULI-DATA.

## Agentic operating model

Provide specialized agents for lead intelligence, sales development, account intelligence, deal intelligence, marketing, support, customer success, finance, procurement, inventory, manufacturing, supply chain, HR, projects, tax/compliance and enterprise intelligence.

Every agent action must be scoped by identity, tenant, company, plant, permission, tool, context and resource policy. Store provenance and execution outcomes. Agents may research, summarize, classify, draft, recommend and prepare transactions. Require configured approval/explicit authorization before sending external messages, publishing campaigns, posting journals, releasing payments, approving POs, issuing quotations/invoices, disbursing loans, changing contractual commitments, changing permissions or deleting statutory/customer records. Never bypass maker-checker or segregation-of-duties controls.

Use MOULI-RAG through MOULI-DATA for grounded retrieval. Every material AI insight must cite source records and distinguish verified facts from inference and forecast. Never cross tenant boundaries.

## Intelligence and optimization

Provide analytics across revenue, pipeline, margin, cash, working capital, inventory, procurement, manufacturing, quality, workforce, projects, customer health, retention, support and agent effectiveness. Add explainable expert rules, fuzzy inference and genetic/heuristic optimization as advisory capabilities. Optimization outputs remain proposals until authorized.

## Architecture and implementation constraints

- Rust application, one executable and exactly one Rust source file: `src/main.rs`.
- No `mod` declarations, separate Rust modules, microservices, sidecars, separate frontend/backend or external application logic.
- Cargo.toml, README, LICENSE, runtime configuration, logs, backups and static assets are permitted.
- One executable supports server, headless, embedded/local and agent/API/MCP modes as capabilities are implemented.
- All persistence must go through MOULI-DATA. Do not add PostgreSQL, Redis, SQLite, pgvector or another competing store.
- The current implementation exposes a clearly isolated MOULI-DATA HTTP adapter boundary. Its endpoint contract is provisional and must be reconciled against the actual MOULI-DATA API before production use.
- Authentication and authorization are delegated to MOULIONE. The current server expects to run behind a trusted MOULIONE gateway which injects authenticated context headers; it is not safe to expose directly to the public internet without that gateway and verified authorization enforcement.
- External integrations must use documented, authorized APIs, explicit credentials, scoped permissions, rate limits, idempotency, retries and audit evidence.
- Financial and lending calculations must be deterministic, decimal-safe, versioned and independently tested. Do not use floating-point arithmetic for money.
- Never claim a subsystem is complete because a route, screen or type exists.

## Delivery sequence

1. Inspect and inventory MOULI, MOULIONE, MOULI-DATA, MOULI-RAG, AgentBox and existing CRM/ERP repositories.
2. Verify the actual MOULIONE identity/authorization and MOULI-DATA APIs; implement adapters only against verified contracts.
3. Establish canonical master-data identifiers and tenant/company/legal-entity/plant scoping.
4. Implement CRM foundation: organizations, contacts, leads, accounts, opportunities, activities and Customer 360.
5. Implement ERP foundation: master data, finance ledger, procurement, inventory, order management and audit references.
6. Implement cross-domain transactions and durable workflow orchestration with idempotency, reconciliation and compensating actions.
7. Add service, marketing, customer success, manufacturing, warehouse, supply chain, HR, projects, tax and industry packs.
8. Add agent tools, approval gates, grounded RAG and analytics.
9. Add authorized integrations, desktop/native UI and MCP surfaces without violating the one-executable/one-source-file constraint.
10. Run formatting, compile, clippy, unit, integration, security, isolation, migration, backup/restore and performance checks.

## Production-readiness reporting

After each iteration report an evidence-backed percentage and per-domain status. Classify each capability as Implemented, Compiled, Unit Tested, Integration Tested, Real-world Verified, Partial, Not Implemented or Blocked. Report test commands/results, actual integration contracts verified, security/isolation evidence, performance measurements, known limitations and blockers. Do not invent a percentage or claim production readiness without evidence.

## Current implementation status

The initial code is a Rust server foundation with business-resource validation and a provisional MOULI-DATA adapter. It is not a complete CRM or ERP. The MOULI-DATA endpoint contract, MOULIONE gateway enforcement, domain-specific transaction semantics, UI, agent execution, and production integration tests remain to be verified and implemented.
