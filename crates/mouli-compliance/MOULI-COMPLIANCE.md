# MOULI-COMPLIANCE — Master Implementation Prompt
**Category:** Governance, risk and compliance  
**Product mission:** Provide a unified governance, risk and compliance platform for managing regulatory frameworks, controls, evidence, assessments, privacy obligations, risk and vendor compliance, with auditable workflows and AI assistance under human review.

## Product capabilities
- Regulatory and compliance framework library, requirements, obligations and cross-framework mappings
- Control library, control testing, evidence collection, evidence retention and audit-ready evidence packages
- Compliance and framework assessments, findings, remediation tasks and follow-up verification
- Risk register, risk assessment, treatment plans and residual-risk tracking
- Privacy and data-protection operations: processing activities and purposes, consent, data-subject requests, privacy impact assessments, breach notification, cross-border transfers, retention and legal holds, with jurisdiction rules (for example India DPDP Act, GDPR) held as configurable, dated frameworks
- Vendor and contract compliance assessment and monitoring
- Policy lifecycle: authoring, approval, attestation, exceptions and review cycles
- Regulatory change monitoring and impact analysis against existing obligations and controls
- Compliance dashboards, deadlines, SLAs, escalation and regulatory reporting
- AI assistance for mapping, gap analysis and summarization with citations to source requirements and mandatory human review

## Ownership boundaries
MOULI-COMPLIANCE owns frameworks, requirements, obligations, controls, evidence, assessments, findings, privacy registers and compliance reporting. MOULI-AUTH owns identity, authorization, tenancy, workflow, approvals and audit authority; MOULI-DATA owns persistence and retrieval; MOULI-AGENTS owns agent definition and workflow design, with AgentBox executing agents; MOULI-SECURITY owns security-operations workflows and detection; MOULI-LEGAL owns matters and contracts. Do not build a competing workflow engine, identity system or database. The software supports compliance work; it does not give legal advice, certify compliance or replace qualified review. A control or assessment is passed only by a recorded human attestation backed by evidence, never by an AI verdict. Keep jurisdiction, effective dates and source citations on every requirement.

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
