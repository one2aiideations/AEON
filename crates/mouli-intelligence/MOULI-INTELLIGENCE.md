# MOULI-INTELLIGENCE — Master Implementation Prompt
**Category:** Analytics and decision intelligence  
**Product mission:** Provide shared business intelligence, analytics, forecasting and decision support across MOULI products.

## Product capabilities
- Semantic business metrics and KPI catalog
- Dashboards, reports and drill-down analysis
- SQL, graph, vector and time-series analytics through MOULI-DATA
- Data preparation and governed analytical models
- Forecasting, scenario and what-if analysis
- Anomaly detection and root-cause exploration
- Executive and operational intelligence
- Natural-language analytics and evidence-backed answers
- Data lineage, metric definitions and quality monitoring
- Agent-assisted insights and decision workflows

## Ownership boundaries
MOULI-DATA owns data storage and retrieval; MOULI-INTELLIGENCE owns analytical experiences, metrics, models and decision-support workflows. Do not duplicate source-of-truth records.

## Mandatory engineering and delivery rules

1. Inspect the existing repository, source code, tests, deployment and documentation before changing anything.
2. Reuse working code; identify duplicates and gaps before replacing components.
3. For this product application, use one Rust executable and exactly one Rust source file, `src/main.rs`. Organize logical components as internal modules, structs, traits, enums and functions inside that file. Do not introduce separate Rust source files, microservices, sidecars, Electron or Flutter runtimes.
4. This single-file rule applies to the product application, not to external infrastructure or explicitly named existing platforms that must be integrated (for example AgentBox).
5. Integrate with MOULIONE for identity, authorization, tenant and governance authority.
6. Integrate with MOULI-DATA for persistence, search and data capabilities. Do not create another general-purpose database.
7. Use MOULI runtime for model execution and MOULI-AGENTS/AgentBox for agent execution where appropriate. Do not create competing foundational runtimes.
8. Define stable internal interfaces and versioned external API contracts. Keep provider-specific integrations behind adapters.
9. Build real business logic, persistence, authorization, error handling, observability and tests. A screen, stub, mock or sample record is not a completed capability.
10. Never claim an integration, test, deployment, transaction or production capability succeeded without direct evidence.
11. Protect secrets; never hardcode API keys, passwords or tokens. Apply least privilege, tenant isolation and audit logging.
12. Require explicit approval for consequential or irreversible actions.
13. Provide a migration strategy and preserve existing user data.
14. Prefer incremental implementation with a working vertical slice before broad feature expansion.

## Required implementation workflow

### Phase 0 — Repository and architecture audit
- Identify the correct repository and branch.
- Inspect all source files, schemas, APIs, tests, workflows and deployment configuration.
- Map existing features to this prompt.
- Identify reusable code, duplicate authorities, mock functionality and blockers.
- Produce a concise audit before destructive changes.

### Phase 1 — Working foundation
- Build the Rust executable.
- Establish MOULIONE authorization integration.
- Establish MOULI-DATA persistence integration.
- Implement the minimum end-to-end business workflow.
- Add structured logs, health checks and error handling.

### Phase 2 — Domain capabilities
- Implement the modules in priority order.
- Use explicit domain types, state machines, validation and invariants.
- Add audit events and idempotency for state-changing operations.
- Integrate external providers only through tested adapters.

### Phase 3 — User experience and automation
- Build role-specific views and workflows.
- Add AI assistance only where it improves a defined task.
- Ensure AI suggestions are distinguishable from verified facts and executed actions.
- Add human approval gates for high-impact actions.

### Phase 4 — Production hardening
- Automated unit, integration, regression and security tests.
- Migration, backup and restore validation.
- Performance and concurrency tests.
- Failure and recovery tests.
- Deployment and operational runbooks.

## Production-readiness report — mandatory after every iteration

Report an evidence-backed percentage and status for each subsystem. For every claim include the evidence source: test name/result, code path, API response, integration test, deployment log or reproducible manual check.

Use these statuses:
- Implemented and tested
- Implemented but not adequately tested
- Partial
- Prototype/mock
- Not implemented
- Blocked by external dependency
- Production validated

Include:
- Overall readiness percentage and calculation method.
- Subsystem readiness percentages.
- Passing/failing tests.
- Real versus mocked integrations.
- Persistence and authorization evidence.
- Security findings.
- Known limitations and blockers.
- Exact next actions.

Do not inflate percentages. A feature is not production-ready merely because a UI or endpoint exists.


## Definition of done
- The application builds as the required single Rust executable.
- The core user journey works end-to-end with real persistence.
- Identity, authorization and tenant isolation are enforced.
- Domain invariants and audit records are tested.
- External dependencies and limitations are explicitly documented.
- Production-readiness report is evidence-backed.

## Implementation instruction
Treat this document as the product scope and acceptance baseline. First inspect the repository and return the audit. Then implement the highest-value complete vertical slice. Do not claim completion until the acceptance evidence exists.
