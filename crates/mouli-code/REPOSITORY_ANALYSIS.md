# MOULI-CODE — Repository Analysis (Phase 1)

**Repository:** `CMKIXVI/mouliapp`  
**Target folder:** `code/`  
**Product:** MOULI-CODE  
**Review date:** 2026-11-22  
**Status:** Initial evidence-backed inspection; implementation is intentionally blocked pending approval.

## Executive finding

The repository is a multi-product container, not an existing unified Rust application. The inspected `main` branch contains at least three independently packaged Rust applications:

- `mail/` — MOULI-MAIL
- `studio/` — MOULI-STUDIO
- `project/` — MOULI-PROJECT

Each inspected product has its own `Cargo.toml` and `src/main.rs`. Their binaries, dependencies, persistence models, UI shells and licenses differ. The requested MOULI-CODE architecture therefore requires a deliberate consolidation/migration; it is not a matter of renaming a root package.

The root `README.md`, root `Cargo.toml`, root `src/main.rs`, and root `LICENSE` were not found during direct path checks. Repository metadata reports default branch `main`, private visibility, and push/admin permission. The repository metadata size field reported 0 despite readable committed subdirectory files, so it is not treated as a reliable file-count measure.

## Inspected evidence

| Area | Evidence inspected | Observed state |
|---|---|---|
| Repository | GitHub repository metadata; `main` branch; direct path probes | Private repository; default branch `main`; no root Rust application files found |
| MOULI-MAIL | `mail/README.md`, `mail/MASTER_ENGINEERING_PROMPT.md`, `mail/Cargo.toml`, `mail/src/main.rs` | Rust/Axum/Tokio/SQLx application; password authentication, opaque bearer sessions, account-scoped mailbox provisioning, local SMTP submission prototype and SQLite persistence are described/visible |
| MOULI-STUDIO | `studio/README.md`, `studio/MASTER_SPEC.md`, `studio/Cargo.toml`, `studio/src/main.rs` | eframe/egui desktop shell; editable local text workspace for Document/Knowledge; JSON-backed project save/open; other creative workspaces are explicitly navigation/foundation only |
| MOULI-PROJECT | `project/README.md`, `project/Cargo.toml`, `project/src/main.rs` | eframe/egui desktop app with SQLite project/task persistence, task status transitions, Kanban-style view, task register/timeline, dependency validation and tests |

This is not yet a complete file-by-file repository inventory. The available GitHub connector operations in this inspection did not expose a recursive repository tree listing, and code search returned no indexed matches. Do not interpret uninspected paths as absent.

## Existing application boundaries and duplication

| Existing capability | Current location | MOULI-CODE destination | Preliminary disposition |
|---|---|---|---|
| Desktop shell/navigation | `studio/src/main.rs`, `project/src/main.rs` | Common shell; Tools; Lines of Intelligence | Consolidate after UI behavior and state models are compared |
| AI/creative workspace navigation | `studio/src/main.rs` | Tools | Retain workspace taxonomy; do not claim engines are implemented |
| Project/task persistence | `project/src/main.rs` | Tools → Project Management; Enterprise Intelligence | Preserve domain logic and tests; migrate deliberately |
| User registration/login/session | `mail/src/main.rs` | Systems → Identity and Workspace | Candidate reuse after security review; not yet enterprise RBAC |
| Mailbox/account persistence | `mail/src/main.rs` | Tools / integrations | Reuse only if mail is in MOULI-CODE scope; avoid duplicating mail as a second authority |
| SQLite persistence | All three inspected apps | Systems → Storage and Database | Consolidate behind one persistence abstraction; schema migration and data compatibility required |
| Rust executable and source layout | Each product independently | One `mouli-code` executable; one `src/main.rs` | Must be unified; current three independent binaries do not satisfy target invariant |
| Branding | MOULI-MAIL, MOULI-STUDIO, MOULI-PROJECT | MOULI-CODE | Normalize user-facing identity after mapping product boundaries |
| Licensing | Mail/project declare MIT; Studio declares Apache-2.0; no root LICENSE found | Root license and notices | Legal/license review required before copying or combining code |

## Build and test status

No Cargo commands were run against the repository in this inspection. Therefore compile status, test status, clippy status, release-build status, runtime behavior and production readiness remain **unverified**. README statements and embedded tests are useful evidence of intended behavior, not proof that the current branch builds or passes tests.

No production-readiness percentage is assigned. UI presence is not counted as functional implementation.

## Risks and migration constraints

1. **Three separate applications:** the three `main.rs` files cannot remain as separate application entry points under the one-executable requirement.
2. **Dependency reconciliation:** mail uses Axum/Tokio/SQLx and server-oriented dependencies; Studio/Project use eframe/egui and different SQLite/serialization dependencies. Feature flags and platform build constraints need a unified dependency plan.
3. **UI/runtime composition:** server/headless modes and native desktop mode must share one runtime without making desktop dependencies mandatory for headless builds.
4. **Persistence:** the three products have separate schemas and database paths. Consolidation requires explicit schema ownership, migration/versioning, backup and rollback strategy.
5. **Security:** mail's README explicitly scopes its SMTP listener to local development and notes missing TLS/STARTTLS, abuse controls, remote delivery and full MIME compliance. It must not be represented as production mail infrastructure.
6. **Feature truthfulness:** Studio explicitly labels most media/office engines as unimplemented; Project explicitly lists missing Gantt/critical path, collaboration, AI, integrations and other capabilities. These must remain classified as partial/not implemented until tested.
7. **Licensing:** preserve attribution and reconcile MIT and Apache-2.0 obligations before code movement.
8. **Single-file constraint:** final application logic and tests must be in one `src/main.rs`; Cargo manifest, README, license/notice and runtime data remain separate non-source files.

## Proposed target architecture (conceptual sections inside one file)

- Application bootstrap and mode selection
- Configuration and diagnostics
- Shared application state and event model
- Desktop shell: exactly Home, Systems, Tools, Lines of Intelligence
- Identity, sessions, organizations, workspaces and permission checks
- Persistence abstraction and SQLite migrations
- AI gateway, provider registry, privacy scrubber and prompt/context services
- Agent registry/runtime and scoped tool authorization
- Workflow scheduler and background execution
- Knowledge ingestion, indexing and retrieval
- Developer tools: editor, terminal, Git, database and API workspaces
- Productivity/creative workspace adapters
- Lines of Intelligence domain registry and workspace metadata
- Embedded HTTP/API server and operational endpoints
- Billing/payment interfaces (provider integrations only when verified)
- Marketplace manifests, validation and permissions
- Audit, telemetry, recovery and administration
- Tests colocated in `src/main.rs`

These are logical sections, not Rust modules. No subsystem is asserted to exist until implementation and tests prove it.

## Migration sequence proposed

1. Complete recursive tree and file-coverage ledger; inspect all source manifests, source files, tests, assets, scripts, CI and deployment files.
2. Produce full feature inventory, dependency map, duplicate/dead-code report and test/build baseline for each product independently.
3. Approve preservation, licensing and data-migration strategy.
4. Approve the single-file Rust architecture, feature flags, runtime modes, persistence authority and four-section UI information architecture.
5. Create the MOULI-CODE application shell and build/test it before migrating product capabilities.
6. Migrate one verified capability at a time, preserving behavior and adding regression tests.
7. Run format, check, clippy, test and release build; perform security and production-readiness reviews with evidence.

## Approval gate

Per the supplied MOULI-CODE master engineering prompt, Phase 1/2/3 findings and the migration architecture must be presented and approved before application source code is written. This commit adds documentation only; it does not alter or delete existing application code.

**Next required decision:** approve completion of the full file-by-file inventory and architecture normalization, or provide corrections to the proposed scope and consolidation order.
