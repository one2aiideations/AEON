# mouli-biz

The MOULI-BUSINESS monolith (v0.13): a local-first CRM/ERP core in one Rust binary with SQLite
storage. It was ported from `AEON-BUSINESS.rs`; the only changes are compile fixes, lint cleanup
and three corrected test assertions (see the commit message).

What it holds: customers, transactions, memory and events; a small trainable decision policy with
versions and rollback; approvals; a hash-chained audit log; universal business entities, events,
workflows and rules; knowledge search (lexical plus local-embedding ranking); a double-entry
ledger with periods, AR/AP and aging; and a private peer network (Ed25519 identities, encrypted
knowledge sync). The AI runtime is loopback-only (`MOULI_OLLAMA_ADDR`, default `127.0.0.1:11434`).

```
mouli-biz test                      # 70+ checks: ledger, audit chain, isolation, sync, training
mouli-biz --db demo.db business-demo   # load demo organisation, customers, orders
mouli-biz --db demo.db serve 127.0.0.1 8787   # HTTP API; GET / for the page
```

Useful routes: `/health`, `/dashboard`, `/audit/verify`, `/finance/trial-balance?currency=INR`,
`/business/entities`, `/ai/knowledge/search`, `/predict`.

Not production ready: no authentication on the HTTP API (keep it on loopback), the crypto is
used as published by the RustCrypto crates but has had no review here, and this crate is not yet
part of the OS image.
