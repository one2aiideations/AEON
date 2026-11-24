# MOULI-CONNECT

> **Purpose:** Provide unified messaging, voice/video, collaboration and social communication experiences for individuals, teams and enterprises.

This folder is the product-level specification and implementation guide for **MOULI-CONNECT**. It expands the product mandate into an engineering reference. The named source specification remains authoritative for detailed product requirements; this README adds navigation, boundaries, lifecycle expectations and verification guidance without replacing the source prompt.

## 1. Product mandate and scope

Provide unified messaging, voice/video, collaboration and social communication experiences for individuals, teams and enterprises.

The implementation must deliver real, end-to-end workflows. A route, screen, sample dataset, generated response or placeholder is not a completed capability. Every function exposed to users must have a defined owner, permission model, persistence behavior, failure behavior and test evidence.

## 2. Canonical ownership

MOULI-CONNECT owns communication product workflows, not enterprise identity, general storage or agent execution. MOULIONE governs access; MOULI-DATA persists messages and artifacts; MOULI-AGENTS/AgentBox may provide scoped assistants.

**Folder owner:** Conversations, channels, contacts, presence, meetings, shared media, feeds, communities, notifications and communication UX.

This product must consume shared platform authorities through versioned interfaces. It must not introduce a competing identity provider, tenant registry, general-purpose database, audit authority, workflow/approval authority or agent execution runtime.

## 3. Functional capability map

- Direct/group messaging, contacts, presence, workspaces, channels and threaded collaboration
- Voice/video calls, meetings, conferencing and screen-sharing workflows
- File and media sharing with permission-aware access
- Posts, feeds, profiles, stories, groups, events, reactions and moderation
- Search across conversations and shared content
- AI summaries, translation, transcription and communication assistance
- Multi-device synchronization, notification preferences and privacy controls
- Official third-party communication adapters where APIs permit

Each capability should be tracked independently through discovery, design, implementation, automated testing, integration testing, operational validation and production acceptance. Do not mark a whole capability complete because one sub-flow or user interface exists.

## 4. Primary end-to-end workflows

1. Resolve participants and workspace access through MOULIONE
2. Create conversation/channel and enforce membership permissions
3. Send and synchronize messages with durable delivery and read-state semantics
4. Create meetings and manage invitations, attendance and artifacts
5. Apply moderation/reporting controls and retain auditable administrative events
6. Generate AI summaries only from content the requester is authorized to access

For every workflow, document: initiating actor, tenant and organizational context, required permissions, input schema, validation rules, state transitions, persistence effects, emitted events, audit records, external dependencies, retry/idempotency behavior, error paths and completion evidence.

## 5. Domain and data design

- Define explicit domain entities, identifiers, relationships, enumerations and lifecycle states.
- Separate authoritative records from derived views, caches, search indexes and analytics projections.
- Enforce tenant scope in repository/service boundaries; never rely only on UI filtering.
- Validate all external input and all state transitions at the server boundary.
- Use stable identifiers, schema/version metadata and documented migration behavior.
- Make state-changing requests idempotent where retries are possible.
- Preserve provenance for imported, AI-generated, externally synchronized and human-approved information.
- For financial values use exact decimal or integer minor units; never use binary floating point as authoritative money.
- Define retention, deletion, export, backup and restore behavior before production use.

## 6. Shared platform integration

| Shared authority | Required interaction |
|---|---|
| **MOULIONE / auth** | Authenticate actors; resolve tenant and company/plant context; authorize every protected action; enforce approvals and write audit evidence. |
| **MOULI-DATA** | Persist and retrieve domain data using tenant-aware contracts; use native RAG when grounded retrieval is needed. |
| **MOULI runtime** | Request model inference through provider-neutral, observable interfaces; protect prompts and sensitive context. |
| **MOULI-AGENTS / AgentBox** | Define governed agent capabilities in MOULI-AGENTS; send actual execution to AgentBox; scope tools and permissions. |
| **Other MOULI products** | Integrate through documented, versioned adapters and canonical references; do not duplicate another product's source of truth. |

Every cross-product request should carry a correlation identifier and the necessary actor/tenant/authorization context. Never trust caller-supplied identity headers unless they are established by a trusted gateway and cryptographically or operationally protected.

## 7. API and integration contracts

- Publish versioned request/response schemas and explicit error codes.
- Specify authentication, authorization, tenant resolution, pagination, filtering, sorting and rate limits.
- Define idempotency keys for create/payment/order/approval or other retry-sensitive commands.
- Keep provider-specific behavior behind adapters; isolate credentials and support rotation.
- Set connection, read and total timeouts; define retry budgets and circuit-breaker behavior.
- Validate webhook signatures, replay windows and event ordering where applicable.
- Document compatibility, deprecation and migration policy before changing public contracts.

## 8. Security, privacy and governance

- Default deny and least privilege; enforce authorization server-side on every operation.
- Isolate tenant data across reads, writes, exports, search and background jobs.
- Store secrets only in approved secret-management facilities; never commit credentials or tokens.
- Encrypt sensitive data in transit and at rest according to deployment controls.
- Record security-relevant actions without unnecessarily logging personal, financial or confidential payloads.
- Require explicit approval for consequential, irreversible or externally visible actions.
- Distinguish AI suggestions, human decisions and executed transactions in both UI and audit history.
- Protect against prompt injection and untrusted document/tool output when AI features are enabled.
- Define access review, incident response, data retention and breach-handling procedures.

## 9. Reliability and operations

- Provide structured logs, metrics, traces, correlation IDs, health and readiness checks.
- Define service-level objectives for latency, availability, throughput and recovery where relevant.
- Bound concurrency, queue depth, payload size, memory use and external API calls.
- Make background jobs observable, resumable and safe to retry.
- Provide backup/restore and disaster-recovery procedures with tested recovery points.
- Document deployment configuration, migrations, rollback, key rotation and operational runbooks.
- Degrade safely when a model provider or external adapter is unavailable; core deterministic workflows must not silently depend on AI.

## 10. Engineering delivery sequence

1. **Repository audit:** verify repository, branch, existing code, workflows, tests, data schemas, deployment and current production behavior.
2. **Capability mapping:** map each source-prompt requirement to implemented code, tests, partial work, mocks or gaps.
3. **Architecture validation:** confirm the canonical authority boundaries and identify duplicate ownership before adding code.
4. **Vertical slice:** deliver one complete, useful user journey with real authorization, persistence, validation, audit and tests.
5. **Domain expansion:** implement remaining capabilities in dependency order with explicit state machines and migrations.
6. **Integration:** connect shared MOULI authorities and external providers through tested adapters.
7. **Hardening:** run security, concurrency, performance, recovery, migration and interoperability checks.
8. **Release:** publish operational documentation, deployment evidence, rollback instructions and a readiness report.

## 11. Test and acceptance matrix

| Test layer | Required evidence |
|---|---|
| Unit | Domain validation, invariants, policy decisions and state transitions pass deterministic tests. |
| Integration | Real persistence adapter, authentication context and service contracts are exercised. |
| Contract | Versioned APIs and provider adapters pass schema/compatibility tests. |
| Security | Tenant isolation, authorization bypass, secret handling, injection and audit controls are tested. |
| Resilience | Timeouts, retries, duplicate requests, partial failures and recovery are verified. |
| End-to-end | Critical user journeys complete with real dependencies, not fixtures alone. |
| Operations | Health/readiness, logs, metrics, backup/restore and deployment rollback are demonstrated. |

## 12. Production-readiness reporting

After every implementation iteration, report an evidence-backed percentage and status by subsystem. Use only these statuses: **Implemented and tested**, **Implemented but not adequately tested**, **Partial**, **Prototype/mock**, **Not implemented**, **Blocked by external dependency**, or **Production validated**.

Include the calculation method, passing/failing test names, code paths, real-versus-mocked integrations, authorization and persistence evidence, security findings, operational evidence, known limitations, blockers and exact next actions. Do not inflate readiness. A successful compile or deployed UI alone is not production validation.

## 13. Source files and maintenance

- **Source specification:** [MOULI-CONNECT.md](./MOULI-CONNECT.md)
- Keep the source prompt intact unless a deliberate, reviewed specification change is requested.
- Update this README when scope, interfaces, ownership or implementation status materially changes.
- Record architectural decisions and breaking contract changes in version-controlled documentation.
- Keep examples clearly marked as examples; never place production secrets or real customer data in documentation.

## 14. Current implementation status

This README documents the intended scope and acceptance standard; it does not certify that the capabilities above are implemented. Maintain a feature-by-feature status table backed by repository paths and test results. Until those checks are performed, the correct status is **specification documented; implementation readiness unverified**.
