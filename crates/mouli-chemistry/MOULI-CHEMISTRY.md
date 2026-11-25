# MOULI-CHEMISTRY — Master Implementation Prompt
**Category:** Scientific discovery: chemistry  
**Product mission:** Provide an autonomous chemical discovery platform for new molecules, reactions and processes, with mandatory hazard screening and human-approved synthesis.

## Product capabilities
- Molecule and reaction records with provenance and versioned datasets
- Property prediction for molecules (physical, chemical, spectroscopic) through models and physics adapters
- Reaction prediction, retrosynthesis and route ranking by cost, yield, safety and green-chemistry score
- Reaction-condition optimisation and process simulation
- Hazard, toxicity, stability and regulatory screening (for example REACH-style restrictions) before anything is ranked or exported
- Surface chemistry, catalysis and crystal-structure modelling through adapters
- Knowledge graph of compounds, reactions, sources and results
- Literature and patent mining with citations
- Autonomous campaign loop with budgets, approvals and complete audit
- Laboratory automation handoff and result ingestion

## Ownership boundaries
MOULI-CHEMISTRY owns molecules, reactions, routes, campaigns and evidence. MOULI-AUTH owns identity, authorization, tenancy, workflow, approvals and audit authority; MOULI-DATA owns persistence, retrieval and provenance storage; MOULI-AGENTS owns agent definitions and workflow design, with AgentBox executing agents; the MOULI runtime owns model inference and compute. Do not build a competing identity system, database, workflow engine or agent runtime. Scientific engines (simulators, solvers, provers, lab and cloud services) are used through narrow adapters with stated capabilities and health, never copied or silently replaced. Dual-use safety is a hard requirement, not an option: every candidate is screened against hazard, toxicity and controlled-substance rules before it is shown or ranked; candidates that look intended or likely to cause harm are never generated, optimised or exported, and the refusal is audited; any synthesis, purchase or lab request needs a recorded approval by a qualified person (and a second person where policy says so). Model output is a hypothesis, never a result: a property, activity or safety claim is only as strong as its recorded evidence (calculation, measurement or citation), and predicted values are always labelled as predictions with their uncertainty. It never produces synthesis routes for substances on controlled or prohibited lists, and it does not replace a chemist's or a safety officer's judgement.

## Mandatory engineering and delivery rules

1. Inspect the existing repository, source code, tests, deployment and documentation before changing anything.
2. Reuse working code; identify duplicates and gaps before replacing components.
3. For this product application, use one Rust executable and exactly one Rust source file, `src/main.rs`. Organize logical components as internal modules, structs, traits, enums and functions inside that file. Do not introduce separate Rust source files, microservices, sidecars, Electron or Flutter runtimes.
4. This single-file rule applies to the product application, not to external infrastructure or explicitly named existing platforms that must be integrated (for example AgentBox).
5. Integrate with MOULI-AUTH for identity, authorization, tenant and governance authority.
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
- Establish MOULI-AUTH authorization integration.
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
