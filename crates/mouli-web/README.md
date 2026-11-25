# MOULI-WEB

> **Purpose:** Provide an AI-native web operating environment combining visual website authoring, browsing intelligence, website understanding and compilation, agent-native interaction, publishing and reusable web capabilities.

This folder is the detailed product guide and implementation acceptance reference for **MOULI-WEB**. The linked master specification remains the normative source for detailed requirements. This README expands navigation, domain ownership, end-to-end workflow expectations, integration rules and verification criteria; it does not replace or silently amend the source prompt.

## What exists today

The crate is the rendering and scripting engine behind the Preview window, plus the assistant-side
page tools. The rest of this README is the wider product guide; this section is the honest status.

| Part | Status |
|---|---|
| HTML tokenizer and document tree | Done, with tree editing for scripts. |
| CSS: selectors, `@media`, cascade, `calc`, `var`, colours | Done for the common subset. |
| Layout: block, inline, flex, grid (tracks, `fr`, `repeat`, `auto-fill`, spans, placement), tables, lists, positioning | Done, with floats (`left`/`right`, `clear`; text wraps around them) except that floats do not escape a plain container. Grid areas and `rowspan` are **not** done. |
| Pictures: PNG, JPEG (baseline and progressive), GIF (first frame) | Done. WebP, SVG and animation are not. |
| Browser: address bar, history, links, forms (GET), style sheets, scripts, pictures | Done in the Browser window (Ctrl-B). POST forms, cookies, downloads and tabs are not. |
| JavaScript: syntax of modern scripts, closures, classes, destructuring, exceptions, timers | Done, with step, depth, object and size limits. `Map`, `Set`, `Promise`, regular expressions and `Date` (UTC only) exist. No `async`/`await`, generators or `Symbol` yet. |
| `document`, elements, events (with bubbling), `classList`, `style`, `innerHTML`, selectors | Done. No network, storage or `eval`. |
| Markdown preview | Done. |
| Tabs, bookmarks, publishing, MCP discovery | Not started; see the sections below. |

Everything is tested in `cargo test -p mouli-web`; display tests live in `mouli-screen`. Testing so far
is in QEMU only, not on real hardware.

## 1. Mission and product boundary

Provide an AI-native web operating environment combining visual website authoring, browsing intelligence, website understanding and compilation, agent-native interaction, publishing and reusable web capabilities.

**Folder ownership:** Browser and website inspection UX, visual authoring, source editing, website intelligence/compiler, publishing integrations and MCP capability discovery.

WEB is a single Rust monolithic application per its source specification. It must not create separate MCP servers, independent databases, duplicate identity systems or a separate browser backend service. External browser-engine integration is permitted only as a bounded adapter; MOULI-AUTH and MOULI-DATA remain shared authorities.

The product must deliver complete, testable workflows rather than isolated screens, endpoints, mocks or sample records. Each feature needs a defined actor, permission, data contract, state transition, failure path, audit behavior and acceptance evidence.

## 2. Functional capability catalogue

- Browser navigation, tabs, history, bookmarks, sessions, downloads and page capture
- DOM, metadata, link, form and accessibility-tree inspection
- Visual design/code/split website authoring with editable HTML/CSS/JS source
- Multi-page site structure, reusable components, templates, themes and assets
- Website understanding, extraction, compilation and structured representation
- Preview, test, debug, export and version history
- Publishing/hosting integrations through authorized adapters
- MCP capability discovery and governed website/agent actions
- Reusable web capability marketplace and integration catalog
- Rust-native monolith with browser-engine integration only where required

Treat this list as a capability map, not a claim that every item is implemented. Track each item separately as implemented, tested, partial, mocked, blocked or not started.

## 3. Critical user and system workflows

1. Navigate or import a website and capture its structure and provenance
2. Inspect page DOM, links, forms and accessibility information
3. Create or edit site using visual and source modes
4. Compile/validate responsive output and preview real generated source
5. Authorize agent interaction with a specific site/action scope
6. Publish through approved hosting adapter and verify deployment result

For each workflow, document the initiating actor, tenant context, authorization check, input validation, domain state machine, persistence writes, emitted events, audit records, external dependencies, idempotency strategy, retry behavior, failure handling and user-visible completion state.

## 4. Domain model and data integrity

- Use explicit domain types, stable identifiers, versioned schemas and documented relationships.
- Keep authoritative business records distinct from caches, derived indexes, projections and analytics.
- Enforce tenant isolation at every service, repository, search, export and background-job boundary.
- Validate all external input and legal state transitions; reject unknown or invalid transitions explicitly.
- Use idempotency keys for retryable state-changing operations and deduplicate webhook/event delivery.
- Preserve source provenance and version history for imported, synchronized, AI-generated and human-approved data.
- Define retention, export, correction, deletion, backup and restore semantics.
- Where money is involved, use exact decimal or integer minor units; never binary floating point for authoritative calculations.

## 5. Shared MOULI platform contracts

| Authority | Contract expected by this product |
|---|---|
| **MOULI-AUTH / auth** | Authentication, actor identity, tenant and organization context, authorization decisions, entitlements, workflow, approvals and audit. |
| **MOULI-DATA** | Common persistence, query, search, retrieval, provenance and data lifecycle; MOULI-RAG is native to this subsystem. |
| **MOULI runtime** | Provider-neutral model inference, resource-aware execution, timeout/cancellation and telemetry. |
| **MOULI-AGENTS / AgentBox** | MOULI-AGENTS manages agent definitions, tools and governance; AgentBox executes agent sessions. |
| **Sibling MOULI applications** | Versioned adapters and canonical references; no duplicate source-of-truth ownership. |

All cross-product requests must propagate correlation IDs and trusted actor/tenant/authorization context. Never trust arbitrary caller-supplied identity headers; establish context through the trusted gateway or a verified token.

## 6. API, adapters and interoperability

- Publish versioned schemas, stable error codes and compatibility/deprecation rules.
- Define authentication, authorization, tenant resolution, pagination, filtering, sorting and rate limits.
- Keep external providers behind narrow adapters with explicit capabilities and health status.
- Protect credentials, support rotation and avoid secrets in source, prompts, logs and examples.
- Configure bounded timeouts, retry budgets, circuit breakers and safe degradation.
- Verify webhook signatures, replay windows, event ordering and idempotency where applicable.
- Do not claim protocol, file-format, provider or platform compatibility until interoperability tests pass.

## 7. Security, privacy and AI governance

- Apply default-deny authorization and least privilege on every protected operation.
- Enforce tenant isolation in APIs, database access, search, exports and asynchronous execution.
- Encrypt sensitive information in transit and at rest according to deployment controls.
- Record security-relevant events without logging unnecessary personal or confidential content.
- Require explicit user approval for consequential, irreversible or externally visible actions.
- Clearly distinguish model suggestions, human decisions and executed actions.
- Treat prompts, files, websites, tool outputs and retrieved documents as untrusted input; defend against prompt injection and data exfiltration.
- Scope agent tools and credentials per task; make access revocable and auditable.

## 8. Reliability and operations

- Provide structured logs, metrics, traces, correlation IDs, health and readiness endpoints.
- Bound concurrency, payload sizes, queue depth, memory, provider calls and execution time.
- Make asynchronous jobs observable, cancellable, resumable and safe to retry.
- Define backup/restore, disaster recovery, retention and migration procedures.
- Document deployment configuration, secrets references, key rotation, rollback and incident response.
- Degrade safely when optional AI or external providers are unavailable; core deterministic workflows must remain understandable.

## 9. Engineering delivery sequence

1. **Audit:** verify repository, branch, source files, dependencies, tests, workflows, deployment and existing behavior.
2. **Map:** map every source requirement to code paths, tests, mocks, partial work or gaps.
3. **Validate architecture:** confirm canonical authority boundaries and remove duplicate ownership only through safe, reviewed migrations.
4. **Vertical slice:** implement one complete user journey with real authorization, persistence, validation, audit and tests.
5. **Expand:** add remaining domain capabilities in dependency order with explicit state machines and migrations.
6. **Integrate:** connect MOULI shared authorities and external providers through contract-tested adapters.
7. **Harden:** execute security, performance, concurrency, resilience, migration and recovery tests.
8. **Release:** provide deployment and rollback evidence, operator documentation and an evidence-backed readiness report.

## 10. Verification and acceptance matrix

| Layer | Acceptance evidence |
|---|---|
| Unit tests | Domain validation, invariants, policy logic and state transitions pass. |
| Integration tests | Real persistence and trusted identity/authorization context are exercised. |
| Contract tests | APIs, adapters, events and schema compatibility pass. |
| Security tests | Tenant isolation, access bypass, secret handling and audit controls pass. |
| Resilience tests | Duplicate requests, retries, timeouts, partial failures and recovery are verified. |
| End-to-end tests | Critical journeys complete using real dependencies, not fixtures alone. |
| Operational checks | Health/readiness, telemetry, backup/restore and rollback are demonstrated. |

## 11. Production-readiness reporting

After every iteration, publish an evidence-backed readiness percentage with a stated calculation method and per-subsystem status. Allowed statuses: **Implemented and tested**, **Implemented but not adequately tested**, **Partial**, **Prototype/mock**, **Not implemented**, **Blocked by external dependency**, **Production validated**.

Include passing/failing test names, source paths, real-versus-mocked integrations, authorization and persistence evidence, security findings, deployment/operations evidence, limitations, blockers and exact next actions. Do not infer production readiness from a successful compile, generated UI or scaffold.

## 12. Source specification and document governance

- **Source specification:** [MASTER_ENGINEERING_PROMPT.md](./MASTER_ENGINEERING_PROMPT.md)
- Preserve the full source specification and its original filename.
- Update this README when product scope, ownership, contracts or verified implementation status changes.
- Keep architectural decisions and breaking changes documented and version controlled.
- Mark examples and provisional contracts clearly; never present them as verified production integrations.

## 13. Implementation status

This document defines intended scope and acceptance expectations only. It does not certify implementation completeness. Until each capability is linked to code and reproducible test/deployment evidence, report the status as **specification documented; implementation readiness unverified**.
