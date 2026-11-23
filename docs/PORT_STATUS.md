# C to Rust port status

Source of truth for the C tree: `AEON-main-full` (`core/` 36 files, about 2,900 lines; `userspace/aeond` about 650 lines).
This table maps each C module to where its behaviour lives in Rust. It was built by reading the C headers and the Rust
public APIs. **Differential tests against the C code (same input through both, outputs compared) have not been run**, so
"ported" means the behaviour exists in Rust with its own unit tests, not that equivalence is proven.

| C module | Rust location | Status |
|---|---|---|
| `aeon_tensor`, `aeon_tensor_view`, `aeon_tensor_layout`, `aeon_runtime_memory` | `aeon-tensor` (`layout`, `view`, `arena`) | ported |
| `aeon_tensor_binding`, `aeon_tensor_materialization` | `aeon-model::binding`, `aeon-model::cache::materialize` | ported |
| `aeon_tensor_cache*` (residency, invalidation, policy, materialization) | `aeon-model::cache` (LRU, budget, `invalidate`, `set_budget`, `validate`) | ported; only the LRU policy exists in C and in Rust |
| `aeon_model_index`, `aeon_model_manifest`, `aeon_model_stream`, `aeon_model_storage` | `aeon-model::{index, manifest, source}` | ported |
| `aeon_nvme_storage`, `aeon_nvme_queue_read` | `aeon-model::block` | ported (software block source; no NVMe hardware path) |
| `aeon_expert_store` | `aeon-model::experts` | ported |
| `aeon_transformer_compute` | `aeon-engine::ops` | ported (CPU reference kernels) |
| `aeon_identity`, `aeon_identity_crypto` | `aeon-identity::registry` (`ed25519-dalek`, `sha2`) | ported |
| `aeon_friction` | `aeon-identity::friction` | ported |
| `aeon_semantic_frame`, `aeon_semantic_gate`, `aeon_semantic_wire` | `aeon-identity::{semantic, gate, wire}` | ported |
| `aeon_syscall` (`submit_semantic`), `aeon_process_gate`, `aeon_io_gate`, `aeon_net_policy` | `aeon-identity::gate` | ported |
| `aeon_ebpf_boundary` (classification), `aeon_kernel_adapter` (hooks) | `aeon-identity::hooks` | classification and hook bookkeeping ported; **attaching real eBPF/netfilter hooks stays kernel-side** |
| `aeon_cognitive_core`, `aeon_cognitive_engine` | `aeon-cognition`, `aeon-core` | ported |
| `aeon_runtime`, `aeon_system_embodiment` (actions, sensors, tools, memory region check) | `aeon-runtime::{action, sensors, tools}`, `aeon-tensor::arena` | ported |
| `aeon_detect_cpu_caps` | `aeon-hw` (`parse_cpuinfo`, `probe`, `plan`) | ported (reads `/proc`, not compile-time flags) |
| `aeon_topstar_contract` | `aeon-adapters::topstar`, and the fuller `aeon-topstar` crate | ported |
| `userspace/aeond` | `aeond` | ported; HTTP surface not diffed against C |
| `aeon_kernel_device` (character device and ioctl) | none | **non-Rust by design**: Linux kernel module (track K0/K2) |
| `aeon_uefi_network` (probe, TLS POST) | none | **non-Rust for now**: UEFI boot environment; a `no_std` Rust port belongs to the kernel track |
| `os/` (Buildroot, QEMU scripts) | `os/` | copied; aeond is now the Rust crate (`os/build.sh` builds it static musl); image boot test is the `os-image.yml` workflow |

## What is still open

1. Differential tests (C vs Rust) for the identity, semantic wire and tensor modules, as required by `AEON_RUST_PLAN.md`.
2. Removal of the C tree: not started, and by the plan it needs the owner's confirmation first.
3. The `os/` image build does not yet consume the Rust crates.

## Related documents

`AEON_RUST_PLAN.md` (migration rules), `AEON_OS_ARCHITECTURE.md`, `AEON_OS_ROADMAP.md`, `AEON_KERNEL_STRATEGY.md`,
`AEON_ENGINE_PLAN.md`, `AEON_APP_LAYER.md`, `AEON_THEORY_NATIVE.md`, `EIGHT_SYSTEM_INTEGRATION_ARCHITECTURE.md`.
Those were written in the C era; treat statements about "C core" as historical.

## Main interface (LibreOne)

| Source | Rust | Status |
|---|---|---|
| LibreOne `libreone/` contracts (message, session, intent, artifacts, approval, policy, capabilities, routing, data_washing, audit, workflow, limits, retry, sandbox policy) | `aeon-interface` | ported and tightened; unit tested; **not yet wired to a UI or to any application** |
| LibreOne README scope (multimodal ingestion, connectors, MCP, enterprise identity, observability) | none | product vision only; no code in the source to port |
| `libreone/native/aeon_contract.py` | none | superseded: AEON itself is the native layer |

## Enterprise authority (AEONONE / AEON-AUTH)

| Source | Rust | Status |
|---|---|---|
| `AEON-AUTH.md` capability map: tenancy and org hierarchy, RBAC/ABAC policy, maker-checker with limits and escalation, tamper-evident audit | `aeon-auth` | core built from the spec; 21 unit tests |
| `AEONONE_AGENTBOX_BOUNDARY.md` authorisation handoff (short-lived, audience-bound, tenant-scoped, replay and revocation checks) | `aeon-auth::envelope` | built with HMAC-SHA256; **not the JWKS scheme AEONONE documents** |
| AEONONE repository (`CMKIXVI/aeonone`) | none | **not available** (not in `sources/`, not reachable from this account); nothing derived from its code |
| Login, MFA, SSO, sessions, persistence, admin APIs, policy lifecycle | none | not implemented |

## aeon-gateway (new, not a port)

Newline-delimited JSON over a Unix socket (mode 0600) so applications and agents outside the console can use `aeon-interface` with streamed replies. Own JSON reader/writer, no new dependencies. Open: no real model backend attached (the `Backend` trait is for `aeond`'s llama-server client), caller identity is claimed not authenticated (`aeon-auth` envelopes not wired in), one connection served at a time, never run outside tests.

## aeon-crm (new from spec, not a port)

Domain core from `crates/aeon-crm/AEON-CRM.md` with ideas from the owner's AEONCRM prototype: lead capture/dedupe/scoring/assignment/conversion, governed pipeline with approval for large wins and exact-money forecast, cases with SLAs, renewals, explainable account health, timeline with provenance, consent. 15 unit tests. Gaps: in-memory only (AEON-DATA not wired), string permissions (not `aeon-auth` decisions), library not the single-file executable the spec asks for, no campaigns/journeys, quotes, knowledge base, deal-risk models, import/export/erasure. Not used with real data.

### aeon-crm: aeon-auth gate

`aeon_crm::authorizer::AuthorityGate` hands every CRM permission check to an `aeon-auth` `Authority` (action `crm.*` on a `crm` resource at the tenant's configured org node). With a gate installed, `Ctx.perms` is ignored. One test covers allow, deny-overrides, unknown principal, other tenant and deactivation. Not passed yet: amount and attribute conditions, signed envelopes, `aeon-auth` maker-checker tiers (the CRM keeps its own four-eyes rule).

### aeon-crm: marketing

`aeon_crm::marketing`: segments (leads or contacts, score and source filters), campaigns with `{name}` templates and a minimum gap between contacts, a read-only send plan that lists who is skipped and why (no or withdrawn consent, contacted too recently, already sent), approval of a send by a second person, and execution through a `Channel` trait that re-plans at send time and never double-sends on retry. 5 tests. Not done: journeys, real email/SMS adapters, unsubscribe links, quiet hours, A/B variants, attribution, an agent that drafts campaigns (it would call these same governed operations).
`aeon_crm::authorizer::AuthorityGate` hands every CRM permission check to an `aeon-auth` `Authority` (action `crm.*` on a `crm` resource at the tenant's configured org node). With a gate installed, `Ctx.perms` is ignored. One test covers allow, deny-overrides, unknown principal, other tenant and deactivation. Deal, conversion and renewal amounts are passed so `AmountAtMost` rules work; winning a deal now also needs `crm.opportunity.close` (checked with the amount). Not passed yet: attribute conditions, signed envelopes, `aeon-auth` maker-checker tiers (the CRM keeps its own four-eyes rule).

## aeon-crypto (new): primitives for the in-house TLS client

Decision: AEON writes its own HTTPS client for hosted models instead of adding curl or rustls. This is step 1: HMAC/HKDF (over the `sha2` crate), `HKDF-Expand-Label`, ChaCha20, Poly1305, ChaCha20-Poly1305 and X25519. Checked against RFC 4231, 5869, 7748, 8439 and 8448 vectors, plus 129 randomised vectors from OpenSSL (`tools/gen_vectors.py`, `tests/cross_check.rs`). Not audited for side channels. Still to build, in order: X.509 parsing, ECDSA P-256/P-384 and RSA signature verification, the TLS 1.3 client (handshake, records, certificate and hostname checks), an HTTP/1.1 client with streaming, then key storage and the hosted-model backend. Until certificate verification exists, nothing may connect to the internet.
