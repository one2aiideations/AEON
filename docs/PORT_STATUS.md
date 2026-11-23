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

## os/: persistence (AEON-DATA)

`aeond::fslabel` reads ext4/FAT labels straight from a device (no blkid/udev in the image); `aeond::persist` mounts the filesystem labelled `AEON-DATA` read-write at `/aeon/data` and keeps `settings`, `history/chat-NNNN.log` and a `models/` folder there; the model used last is listed first. Unmounted before power-off. CI attaches an ext4 `AEON-DATA` disk in the BIOS boot test and reads the setting and chat log back with `debugfs`; the UEFI test (no disk) checks the "nothing will be saved" message. Not done: encryption, history size limits, a settings screen, secrets storage (API keys come next), exFAT/NTFS labels, finding the partition when two carry the label.

## os/: wired network

`aeond::net` finds Ethernet adapters in `/sys/class/net` (hardware-backed, not wireless/bridge/tunnel), waits for carrier, runs BusyBox `udhcpc` (its default script sets address, route and resolv.conf) and prints the result; the clock is set from the RTC with `hwclock`. Image changes: kernel NIC and USB-Ethernet drivers, a BusyBox fragment (`ip`, `udhcpc`, `hwclock`, `ping`, `nslookup`), `ca-certificates`, and a post-build check that fails the build if the tools are missing. CI gives the BIOS boot an Intel e1000 and the UEFI boot an e1000e on QEMU's user network and expects `eth0 10.0.2.15/24, dns 10.0.2.3`. Not done: Wi-Fi, static addresses or IPv6, NTP (it would contact the internet, so it waits for the hosted-model step), a retry when a cable is plugged in later, a network screen. Which real NICs work is unverified.

## aeon-crypto (new): primitives for the in-house TLS client

Decision: AEON writes its own HTTPS client for hosted models instead of adding curl or rustls. This is step 1: HMAC/HKDF (over the `sha2` crate), `HKDF-Expand-Label`, ChaCha20, Poly1305, ChaCha20-Poly1305 and X25519. Checked against RFC 4231, 5869, 7748, 8439 and 8448 vectors, plus 129 randomised vectors from OpenSSL (`tools/gen_vectors.py`, `tests/cross_check.rs`). Not audited for side channels. Still to build, in order: X.509 parsing, ECDSA P-256/P-384 and RSA signature verification, the TLS 1.3 client (handshake, records, certificate and hostname checks), an HTTP/1.1 client with streaming, then key storage and the hosted-model backend. Until certificate verification exists, nothing may connect to the internet.

## aeon-pki (certificates for the in-house TLS client, step 2)

Own implementation, no new dependencies beyond the SHA-2 crate already used:

- `bignum`: unsigned big integers (Knuth division, modular power and inverse). Public data only, not constant time.
- `der`: strict DER reader (definite and minimal lengths, no trailing data).
- `x509`: v3 certificates: names, validity, key, SAN (DNS, IP), basic constraints, key usage, extended key usage; unknown critical extensions are refused. Signature algorithms: RSA PKCS#1 v1.5 and PSS, ECDSA, with SHA-256/384/512 (SHA-1 and MD5 are refused). Keys: RSA 2048 to 8192 bits, ECDSA P-256 and P-384.
- `ecdsa`, `rsa`: signature verification.
- `verify`: chain building to a trust store (signatures, validity, CA and path-length rules, server-auth purpose), host name matching by SAN only, wildcard only as a whole left-most label.
- `pem`: PEM bundles, so `/etc/ssl/certs/ca-certificates.crt` loads as the trust store (116 of 120 certificates of a typical bundle load; the rest use unsupported algorithms or curves).

Checked against 288 OpenSSL signature vectors (96 valid, 192 altered), chains generated with OpenSSL (EC, RSA, PSS; wrong name, expired, not yet valid, forged root, missing intermediate, path length, client-only purpose, unknown critical extension, tampering). Not covered yet: name constraints and revocation (CRL/OCSP); a certificate that uses them as critical is refused. Next: the TLS 1.3 client itself.

## aeon-tls (own TLS 1.3 client, step 3)

`Connection::connect(stream, &Config)` runs a TLS 1.3 handshake over any `Read + Write` and then behaves as a stream. One cipher suite (`TLS_CHACHA20_POLY1305_SHA256`), X25519, SNI for DNS hosts. The server's chain is checked with `aeon-pki` (trusted root, validity, CA rules, host name by SAN), the CertificateVerify signature must match the leaf key (ECDSA P-256/P-384, RSA-PSS SHA-256/384), and Finished is verified. Handles tickets (ignored), KeyUpdate in both directions, `close_notify`; a close without it is reported as an error (truncation). Not supported: HelloRetryRequest, resumption, 0-RTT, client certificates, AES-GCM-only servers.

Tested against a real `openssl s_server` over loopback (skipped when openssl is absent): ECDSA and RSA servers, a 1.5 MB transfer with key updates, wrong host name, untrusted root, empty trust store, and a server that only offers AES-GCM (refused). Key-schedule values are exercised end to end by the interop; no separate published trace is checked in. Hand-written crypto: not audited for side channels.

Next: HTTP/1.1 client with streaming (step 4), then key storage and the hosted-model backend (step 5).

## aeon-http (own HTTP/1.1 client, step 4)

`Client::request(&Request)` resolves, connects (timeouts), runs `aeon-tls` for `https` with the system CA bundle (`Client::system()`), and returns the status, headers and a streaming `Body` (`Read`). One request per connection. Bodies: Content-Length, chunked (extensions and trailers accepted), or until close; a truncated body is an `UnexpectedEof` error. Strict parsing: CRLF only, no folded headers, bad or conflicting lengths and Content-Length together with chunked are refused, head size capped. Header injection in requests is refused. Plain `http` is allowed only to loopback; redirects are not followed.

Tested with canned responses (valid and malformed), a loopback server streaming chunked events, a POST echo, and HTTPS to a real `openssl s_server` through the in-house TLS (plus a wrong trust store). Next: API key storage on AEON-DATA and the hosted-model backend (step 5).
