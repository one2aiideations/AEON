# AEON-WEB production-readiness ledger

**Assessment date:** 2026-11-22  
**Release status:** Not production-ready. This is the first executable compiler slice; compilation and runtime tests have not yet been run in this environment because a Rust toolchain is unavailable.

Percentages are subsystem completion estimates against the full master specification, not test-pass rates.

| Subsystem | Readiness | Implemented evidence | Remaining critical work |
|---|---:|---|---|
| Runtime | 20% | Tokio/Axum process, CLI, graceful Ctrl-C shutdown, SQLite initialization | lifecycle diagnostics, unified AEON runtime integration, measured startup/shutdown tests |
| Browser | 5% | Safe HTTP retrieval primitive used by inspection | real browser engine, tabs, sessions, DOM/accessibility inspection, history, downloads, screenshots |
| Studio | 0% | Specification preserved | Design/Code/Split editor, component tree, property inspector, source generation and preview |
| Compiler | 12% | Real HTML fetch/parse, metadata, headings, links, resources, text excerpt, SHA-256 manifest | WebZero parity, CSS/JS/API/workflow analysis, diagnostics, incremental compilation/cache, fixtures |
| World Model | 0% | Website manifest is a first data record | typed entities, tenant ownership, relationships, state/version lifecycle |
| Graph | 0% | None | graph schema, persistence, traversal and impact queries |
| MCP Router | 0% | None | MCP protocol, schemas, discovery, authorization and invocation |
| Agent Engine | 0% | None | planner, verified actions, recovery, human approvals and execution history |
| Integration | 5% | inspect/compile HTTP endpoints and persisted website records | full WebZero API compatibility and drift detection |
| Publishing | 0% | None | static build/export, deployment adapters and verification |
| Marketplace | 0% | None | package registry, licensing, installation, permission and audit flows |
| Identity | 0% | No competing identity authority created | connect to AEON identity and policy authority; remove development tenant label as a trust boundary |
| Security | 10% | loopback default, HTTP(S)-only, redirect revalidation, DNS resolution/pinning, prohibited-IP checks, timeout and size limits | full SSRF test matrix, browser sandbox, authentication, authorization, rate limits, secret handling and security audit |
| Persistence | 15% | SQLite schema, website manifest persistence and compilation audit row | migrations/versioning, tenant isolation, backup/restore, recovery and consistency tests |
| HTTP | 10% | health, inspect, compile and list endpoints | authenticated route policy, complete API contract and regression suite |
| Deployment | 0% | Cargo binary target defined | reproducible release builds, CI evidence, packaging and deployment verification |

## Tests present but not yet executed

- `rejects_private_and_loopback_addresses`
- `extracts_page_title_links_and_headings`
- `refuses_non_http_schemes`

No test pass is claimed. Run in `web/`:

```bash
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Blocking dependency for behavioral migration

The uploaded WebZero Colab notebook does not contain the canonical `webzero-v4.0.24.js` file. It locates that file in mounted Google Drive and invokes its real exports. The full source and representative fixtures are required before the export/route/schema inventory, migration traceability matrix and parity tests can be completed.

## Release gate

Do not expose this server publicly. Authentication and AEON authorization are not connected; `tenant_ref` is a caller-supplied development label, not tenant security. Do not mark any subsystem production-ready until its required tests and operational evidence pass.