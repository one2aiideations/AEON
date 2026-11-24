# MOULI-DATA

> **Purpose:** Provide the common persistence and data-intelligence substrate for the entire MOULI ecosystem, including native retrieval-augmented generation.

This folder is the product-level specification and implementation guide for **MOULI-DATA**. It expands the product mandate into an engineering reference. The named source specification remains authoritative for detailed product requirements; this README adds navigation, boundaries, lifecycle expectations and verification guidance without replacing the source prompt.

## 1. Product mandate and scope

Provide the common persistence and data-intelligence substrate for the entire MOULI ecosystem, including native retrieval-augmented generation.

The implementation must deliver real, end-to-end workflows. A route, screen, sample dataset, generated response or placeholder is not a completed capability. Every function exposed to users must have a defined owner, permission model, persistence behavior, failure behavior and test evidence.

## 2. Canonical ownership

MOULI-DATA is the sole common persistence and retrieval authority. MOULI-RAG is a native MOULI-DATA subsystem, not a separate top-level product. MOULIONE decides who may access data; applications own domain semantics and business rules.

**Folder owner:** Relational, document, key-value, graph, vector, full-text, time-series, event and object storage; ingestion, indexing, retrieval, provenance and data lifecycle.

This product must consume shared platform authorities through versioned interfaces. It must not introduce a competing identity provider, tenant registry, general-purpose database, audit authority, workflow/approval authority or agent execution runtime.

## 3. Functional capability map

- Relational SQL, transactions, constraints, indexes and write-ahead logging
- Document and key-value storage with namespace/catalog/schema management
- Property graph, traversals, path queries and relationship analytics
- Vector collections, embeddings, similarity search and ANN indexes
- Full-text, semantic and hybrid search
- Time-series, event and object/blob storage
- Native MOULI-RAG ingestion, parsing, chunking, embedding, retrieval, reranking and context assembly
- GraphRAG, multi-hop retrieval, entity resolution, knowledge graphs and semantic memory primitives
- Citations, provenance, source attribution, versioning, access filtering and retrieval traces
- Evolutionary retrieval optimization with fitness evaluation, diversity and lineage
- Backup, recovery, encryption, quotas, monitoring and administration

Each capability should be tracked independently through discovery, design, implementation, automated testing, integration testing, operational validation and production acceptance. Do not mark a whole capability complete because one sub-flow or user interface exists.

## 4. Primary end-to-end workflows

1. Register data source, schema and namespace with ownership metadata
2. Ingest documents/events and preserve source provenance and version
3. Apply tenant/actor authorization filters before retrieval
4. Run lexical/vector/graph/hybrid retrieval and rerank candidates
5. Assemble evidence-backed context with citations and trace identifiers
6. Evaluate retrieval quality and optimize only with reproducible test sets

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

- **Source specification:** [MOULI-DATA.md](./MOULI-DATA.md)
- Keep the source prompt intact unless a deliberate, reviewed specification change is requested.
- Update this README when scope, interfaces, ownership or implementation status materially changes.
- Record architectural decisions and breaking contract changes in version-controlled documentation.
- Keep examples clearly marked as examples; never place production secrets or real customer data in documentation.

## 14. Current implementation status

This README documents the intended scope and acceptance standard; it does not certify that the capabilities above are implemented. Maintain a feature-by-feature status table backed by repository paths and test results. Until those checks are performed, the correct status is **specification documented; implementation readiness unverified**.
