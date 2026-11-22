# MASTER ENGINEERING PROMPT — AEON-MAIL
## AI-Native Email Server & Intelligent Mail Client — 100% Rust, Single Monolith

You are the principal Rust systems architect, senior mail-protocol engineer, AI infrastructure engineer, security engineer, and product developer.

Your task is to DESIGN, IMPLEMENT, TEST, AND DELIVER **AEON-MAIL**, a complete AI-native email server and intelligent desktop mail client inspired by Thunderbird, but architecturally designed for the AI-agent era.

This is not a prototype, mockup, proof of concept, or architecture-only exercise.

Build a working, production-oriented email platform.

---

# 1. ABSOLUTE ARCHITECTURAL CONSTRAINTS

## 1.1 Single Monolith
- Entire application implementation must live in `src/main.rs`.
- Exactly one Rust source file; no modules, workspaces, microservices, sidecars, separate frontend/backend, generated sources, or external scripts containing application code.
- Cargo.toml is permitted. Runtime databases, certificates, logs, configuration, and user data may be external.
- One primary executable supports server, desktop client, combined local, headless, administrative, and database-migration modes.

## 1.2 Technology
- Rust, Tokio, Axum, Serde, SQLite via SQLx or equivalent, Rust-native desktop UI preferably egui/eframe, tracing, established cryptography, and tests embedded in `src/main.rs`.
- Avoid unnecessary dependencies.

## 1.3 Product Vision
AEON-MAIL combines Thunderbird-class email client functionality, a standards-compatible mail server, an AI email assistant, agent-native communication, and a persistent email knowledge system. Conventional email must work with AI disabled.

# 2. EMAIL SERVER AND PROTOCOLS

Implement SMTP submission and receiving, authentication, STARTTLS/TLS, outbound delivery, queues/retries, bounce handling, tracking, rate limits, SPF, DKIM, DMARC, and basic spam filtering.

Implement IMAP4rev1 authentication, mailbox operations, retrieval, flags, search, movement/deletion, synchronization, UIDs, IDLE, and MIME handling. Provide a JMAP-compatible API design, CalDAV/CardDAV readiness, MIME attachments, EML and MBOX import/export. Do not claim compatibility before interoperability tests pass.

# 3. THUNDERBIRD-CLASS DESKTOP CLIENT

Provide a functional three-pane interface, unified inbox, multiple accounts, folders, message list/reader, threads, search, compose, attachments, drafts, sent/archive/spam/trash, custom folders, labels, flags, priority, account setup, IMAP/SMTP discovery, OAuth2 readiness, identities/signatures, rich/plain/HTML composition, CC/BCC, reply/reply-all/forward, scheduled send, autosave, keyboard shortcuts, filters/rules, contacts/autocomplete, offline access, synchronization, and notifications. UI controls must perform real operations.

# 4. AI-NATIVE EMAIL ENGINE

AI is optional and provider-independent. Support OpenAI-compatible APIs, Anthropic-compatible integration, Ollama, local endpoints, and other OpenAI-compatible inference servers.

Writing: compose from instructions, rewrite, tone adjustment, summarize drafts, improve clarity, translate, and draft professional responses.

Reading: summarize threads, extract actions/deadlines/commitments/entities/amounts, identify unanswered questions, and suggest replies.

Intelligence: categorization, priority/intent, spam/phishing risk, relationship intelligence, contextual/semantic search, duplicate detection, and conversation summaries.

Automation: suggest rules, classify incoming mail, draft replies, create follow-up reminders, flag attention-needed messages, and daily briefings.

Never silently send, delete, or make external commitments. Require explicit authorization for consequential actions.

# 5. AGENT-NATIVE EMAIL

Provide internal agent capabilities for authorized mailbox reading/search, summarization, extraction, drafting, categorization, task creation, approved workflows, permitted attachment access, and structured responses.

Enforce per-agent identity, explicit scoped permissions, read/write separation, audit trails, approvals, revocable credentials, and execution logs. Never expose unrestricted mailbox access.

# 6. DATABASE AND STORAGE

SQLite initial embedded persistence. Model users, accounts, mailboxes, messages/bodies, attachments, contacts, identities, folders, labels, threads, drafts, rules/filters, AI summaries/embeddings, agent identities/permissions, audit logs, delivery queues, sync state, and configuration.

Provide migrations, foreign keys, transactions, indexes, attachment storage, deduplication, search indexing, backup/restore, integrity checks, and an internal storage abstraction.

# 7. SECURITY AND PRIVACY

Implement secure password hashing, TLS, secure credential storage, sessions, CSRF where applicable, validation, parameterized database access, XSS-safe HTML rendering, HTML sanitization, attachment validation/limits, rate limiting, login throttling, audit logging, authorization checks, and secure defaults.

Never execute scripts or remote content from incoming HTML. Do not send mail content to external AI providers without explicit configuration and authorization. Support local-only AI. Document at-rest encryption limitations accurately; do not claim end-to-end encryption.

# 8. ADMINISTRATION AND OPERATIONS

Provide user creation, mailbox provisioning, domain/SMTP/TLS configuration guidance, queue/delivery monitoring, health, DB/storage statistics, audit viewer, and configuration management. Support Linux and practical Windows/macOS operation, Docker and native binary deployment. Kubernetes must not be required.

# 9. OBSERVABILITY

Structured logs, error reporting, SMTP/IMAP metrics, queue depth/failures, database indicators, sync errors, AI request metrics, resource utilization, and health endpoint. Never log secrets, tokens, or full private email contents.

# 10. IMPLEMENTATION SEQUENCE

Establish project/dependencies; schema/migrations; authentication; SMTP; IMAP; message/MIME storage; Axum API; desktop UI; synchronization; AI providers; agent capabilities; administration; tests; compile and fix; security/reliability checks; deployment documentation.

Continuously compile and test. Prefer a smaller verified implementation over enormous unverified code. No placeholders, TODOs, `todo!()`, `unimplemented!()`, mocks, or fake success on required production paths.

# 11. TESTING

Keep tests in `src/main.rs`. Test registration/authentication, SMTP reception/submission, IMAP operations, MIME and attachments, threading, search, DB transactions, synchronization, AI provider failures/authorization, agent permission enforcement, malicious HTML, invalid attachments, rate limiting, delivery retries, and concurrent mailbox operations.

Run `cargo fmt --check`, `cargo check`, `cargo clippy`, `cargo test`, and a release build; fix failures before claiming completion.

# 12. STRICT SOURCE-CODE POLICY

Repository deliverables: `Cargo.toml`, `src/main.rs`, `README.md`, and LICENSE where appropriate. Exactly one `.rs` file. All application, protocol, frontend, backend, and test logic belongs in `src/main.rs`. Use structs, enums, traits, functions, and clear sections; never split source files.

# 13. PRODUCTION READINESS GATE

Report evidence-based readiness for Rust build, SMTP, IMAP, MIME, database, authentication, desktop UI, synchronization, AI, agent framework, security, and deployment. Distinguish implemented, tested, partial, not implemented, and blocked. Assign percentages based on verified implementation, not code volume. Never report 100% without evidence.

# 14. FINAL DELIVERABLE

Deliver a working AEON-MAIL repository. Report structure, single-file architecture, implemented functionality, protocols, database, AI/agent architecture, security, build/test results, exact run commands, deployment, limitations, and evidence-backed readiness.

## FINAL DIRECTIVE

Build AEON-MAIL as a serious extensible AI-native email platform. Implement, compile, test, and verify; do not substitute architecture descriptions for working code.

**ONE PRODUCT. ONE RUST EXECUTABLE. ONE APPLICATION MONOLITH. ONE SOURCE FILE: `src/main.rs`.**

Product name: **AEON-MAIL**.
