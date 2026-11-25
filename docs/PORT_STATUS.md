# C to Rust port status

Source of truth for the C tree: `AEON-main-full` (`core/` 36 files, about 2,900 lines; `userspace/aeond` about 650 lines).
This table maps each C module to where its behaviour lives in Rust. It was built by reading the C headers and the Rust
public APIs. **Differential tests against the C code (same input through both, outputs compared) have not been run**, so
"ported" means the behaviour exists in Rust with its own unit tests, not that equivalence is proven.

| C module | Rust location | Status |
|---|---|---|
| `aeon_tensor`, `aeon_tensor_view`, `aeon_tensor_layout`, `aeon_runtime_memory` | `mouli-llm::tensor` (`layout`, `view`, `arena`) | ported |
| `aeon_tensor_binding`, `aeon_tensor_materialization` | `mouli-llm::weights::binding`, `mouli-llm::weights::cache::materialize` | ported |
| `aeon_tensor_cache*` (residency, invalidation, policy, materialization) | `mouli-llm::weights::cache` (LRU, budget, `invalidate`, `set_budget`, `validate`) | ported; only the LRU policy exists in C and in Rust |
| `aeon_model_index`, `aeon_model_manifest`, `aeon_model_stream`, `aeon_model_storage` | `mouli-llm::weights::{index, manifest, source}` | ported |
| `aeon_nvme_storage`, `aeon_nvme_queue_read` | `mouli-llm::weights::block` | ported (software block source; no NVMe hardware path) |
| `aeon_expert_store` | `mouli-llm::weights::experts` | ported |
| `aeon_transformer_compute` | `mouli-llm::kernels::ops` | ported (CPU reference kernels) |
| `aeon_identity`, `aeon_identity_crypto` | `mouli-systems::identity::registry` (`ed25519-dalek`, `sha2`) | ported |
| `aeon_friction` | `mouli-systems::identity::friction` | ported |
| `aeon_semantic_frame`, `aeon_semantic_gate`, `aeon_semantic_wire` | `mouli-systems::identity::{semantic, gate, wire}` | ported |
| `aeon_syscall` (`submit_semantic`), `aeon_process_gate`, `aeon_io_gate`, `aeon_net_policy` | `mouli-systems::identity::gate` | ported |
| `aeon_ebpf_boundary` (classification), `aeon_kernel_adapter` (hooks) | `mouli-systems::identity::hooks` | classification and hook bookkeeping ported; **attaching real eBPF/netfilter hooks stays kernel-side** |
| `aeon_cognitive_core`, `aeon_cognitive_engine` | `mouli-agentic::cognition`, `mouli-agentic::kernel` | ported |
| `aeon_runtime`, `aeon_system_embodiment` (actions, sensors, tools, memory region check) | `mouli-agentic::runtime::{action, sensors, tools}`, `mouli-llm::tensor::arena` | ported |
| `aeon_detect_cpu_caps` | `mouli-hw` (`parse_cpuinfo`, `probe`, `plan`) | ported (reads `/proc`, not compile-time flags) |
| `aeon_topstar_contract` | `mouli-agentic::adapters::topstar`, and the fuller `mouli-systems::topstar` crate | ported |
| `userspace/aeond` | `moulid` | ported; HTTP surface not diffed against C |
| `aeon_kernel_device` (character device and ioctl) | none | **non-Rust by design**: Linux kernel module (track K0/K2) |
| `aeon_uefi_network` (probe, TLS POST) | none | **non-Rust for now**: UEFI boot environment; a `no_std` Rust port belongs to the kernel track |
| `os/` (Buildroot, QEMU scripts) | `os/` | copied; moulid is now the Rust crate (`os/build.sh` builds it static musl); image boot test is the `os-image.yml` workflow |

## What is still open

1. Differential tests (C vs Rust) for the identity, semantic wire and tensor modules, as required by `MOULI_RUST_PLAN.md`.
2. Removal of the C tree: not started, and by the plan it needs the owner's confirmation first.
3. The `os/` image build does not yet consume the Rust crates.

## Related documents

`MOULI_RUST_PLAN.md` (migration rules), `MOULI_OS_ARCHITECTURE.md`, `MOULI_OS_ROADMAP.md`, `MOULI_KERNEL_STRATEGY.md`,
`MOULI_ENGINE_PLAN.md`, `MOULI_APP_LAYER.md`, `MOULI_THEORY_NATIVE.md`, `EIGHT_SYSTEM_INTEGRATION_ARCHITECTURE.md`.
Those were written in the C era; treat statements about "C core" as historical.

## Main interface (LibreOne)

| Source | Rust | Status |
|---|---|---|
| LibreOne `libreone/` contracts (message, session, intent, artifacts, approval, policy, capabilities, routing, data_washing, audit, workflow, limits, retry, sandbox policy) | `mouli-interface` | ported and tightened; unit tested; **not yet wired to a UI or to any application** |
| LibreOne README scope (multimodal ingestion, connectors, MCP, enterprise identity, observability) | none | product vision only; no code in the source to port |
| `libreone/native/mouli_contract.py` | none | superseded: MOULI itself is the native layer |

## Enterprise authority (MOULI-AUTH)

| Source | Rust | Status |
|---|---|---|
| `MOULI-AUTH.md` capability map: tenancy and org hierarchy, RBAC/ABAC policy, maker-checker with limits and escalation, tamper-evident audit | `mouli-systems::auth` | core built from the spec; 21 unit tests |
| `AEONONE_AGENTBOX_BOUNDARY.md` authorisation handoff (short-lived, audience-bound, tenant-scoped, replay and revocation checks) | `mouli-systems::auth::envelope` | built with HMAC-SHA256; **not the JWKS scheme MOULI-AUTH documents** |
| MOULI-AUTH repository (`CMKIXVI/aeonone`) | none | **not available** (not in `sources/`, not reachable from this account); nothing derived from its code |
| Login, MFA, SSO, sessions, persistence, admin APIs, policy lifecycle | none | not implemented |

## mouli-gateway (new, not a port)

Newline-delimited JSON over a Unix socket (mode 0600) so applications and agents outside the console can use `mouli-interface` with streamed replies. Own JSON reader/writer, no new dependencies. Open: no real model backend attached (the `Backend` trait is for `moulid`'s llama-server client), caller identity is claimed not authenticated (`mouli-systems::auth` envelopes not wired in), one connection served at a time, never run outside tests.

## mouli-crm (new from spec, not a port)

Domain core from `crates/mouli-crm/MOULI-CRM.md` with ideas from the owner's AEONCRM prototype: lead capture/dedupe/scoring/assignment/conversion, governed pipeline with approval for large wins and exact-money forecast, cases with SLAs, renewals, explainable account health, timeline with provenance, consent. 15 unit tests. Gaps: in-memory only (MOULI-DATA not wired), string permissions (not `mouli-systems::auth` decisions), library not the single-file executable the spec asks for, no campaigns/journeys, quotes, knowledge base, deal-risk models, import/export/erasure. Not used with real data.

### mouli-crm: mouli-auth gate

`mouli_crm::authorizer::AuthorityGate` hands every CRM permission check to an `mouli-systems::auth` `Authority` (action `crm.*` on a `crm` resource at the tenant's configured org node). With a gate installed, `Ctx.perms` is ignored. One test covers allow, deny-overrides, unknown principal, other tenant and deactivation. Not passed yet: amount and attribute conditions, signed envelopes, `mouli-systems::auth` maker-checker tiers (the CRM keeps its own four-eyes rule).

### mouli-crm: marketing

`mouli_crm::marketing`: segments (leads or contacts, score and source filters), campaigns with `{name}` templates and a minimum gap between contacts, a read-only send plan that lists who is skipped and why (no or withdrawn consent, contacted too recently, already sent), approval of a send by a second person, and execution through a `Channel` trait that re-plans at send time and never double-sends on retry. 5 tests. Not done: journeys, real email/SMS adapters, unsubscribe links, quiet hours, A/B variants, attribution, an agent that drafts campaigns (it would call these same governed operations).
`mouli_crm::authorizer::AuthorityGate` hands every CRM permission check to an `mouli-systems::auth` `Authority` (action `crm.*` on a `crm` resource at the tenant's configured org node). With a gate installed, `Ctx.perms` is ignored. One test covers allow, deny-overrides, unknown principal, other tenant and deactivation. Deal, conversion and renewal amounts are passed so `AmountAtMost` rules work; winning a deal now also needs `crm.opportunity.close` (checked with the amount). Not passed yet: attribute conditions, signed envelopes, `mouli-systems::auth` maker-checker tiers (the CRM keeps its own four-eyes rule).

## os/: persistence (MOULI-DATA)

`moulid::fslabel` reads ext4/FAT labels straight from a device (no blkid/udev in the image); `moulid::persist` mounts the filesystem labelled `MOULI-DATA` read-write at `/mouli/data` and keeps `settings`, `history/chat-NNNN.log` and a `models/` folder there; the model used last is listed first. Unmounted before power-off. CI attaches an ext4 `MOULI-DATA` disk in the BIOS boot test and reads the setting and chat log back with `debugfs`; the UEFI test (no disk) checks the "nothing will be saved" message. Not done: encryption, history size limits, a settings screen, secrets storage (API keys come next), exFAT/NTFS labels, finding the partition when two carry the label.

## os/: wired network

`moulid::net` finds Ethernet adapters in `/sys/class/net` (hardware-backed, not wireless/bridge/tunnel), waits for carrier, runs BusyBox `udhcpc` (its default script sets address, route and resolv.conf) and prints the result; the clock is set from the RTC with `hwclock`. Image changes: kernel NIC and USB-Ethernet drivers, a BusyBox fragment (`ip`, `udhcpc`, `hwclock`, `ping`, `nslookup`), `ca-certificates`, and a post-build check that fails the build if the tools are missing. CI gives the BIOS boot an Intel e1000 and the UEFI boot an e1000e on QEMU's user network and expects `eth0 10.0.2.15/24, dns 10.0.2.3`. Not done: Wi-Fi, static addresses or IPv6, NTP (it would contact the internet, so it waits for the hosted-model step), a retry when a cable is plugged in later, a network screen. Which real NICs work is unverified.

## mouli-crypto (new): primitives for the in-house TLS client

Decision: MOULI writes its own HTTPS client for hosted models instead of adding curl or rustls. This is step 1: HMAC/HKDF (over the `sha2` crate), `HKDF-Expand-Label`, ChaCha20, Poly1305, ChaCha20-Poly1305 and X25519. Checked against RFC 4231, 5869, 7748, 8439 and 8448 vectors, plus 129 randomised vectors from OpenSSL (`tools/gen_vectors.py`, `tests/cross_check.rs`). Not audited for side channels. Still to build, in order: X.509 parsing, ECDSA P-256/P-384 and RSA signature verification, the TLS 1.3 client (handshake, records, certificate and hostname checks), an HTTP/1.1 client with streaming, then key storage and the hosted-model backend. Until certificate verification exists, nothing may connect to the internet.

## mouli-pki (certificates for the in-house TLS client, step 2)

Own implementation, no new dependencies beyond the SHA-2 crate already used:

- `bignum`: unsigned big integers (Knuth division, modular power and inverse). Public data only, not constant time.
- `der`: strict DER reader (definite and minimal lengths, no trailing data).
- `x509`: v3 certificates: names, validity, key, SAN (DNS, IP), basic constraints, key usage, extended key usage; unknown critical extensions are refused. Signature algorithms: RSA PKCS#1 v1.5 and PSS, ECDSA, with SHA-256/384/512 (SHA-1 and MD5 are refused). Keys: RSA 2048 to 8192 bits, ECDSA P-256 and P-384.
- `ecdsa`, `rsa`: signature verification.
- `verify`: chain building to a trust store (signatures, validity, CA and path-length rules, server-auth purpose), host name matching by SAN only, wildcard only as a whole left-most label.
- `pem`: PEM bundles, so `/etc/ssl/certs/ca-certificates.crt` loads as the trust store (116 of 120 certificates of a typical bundle load; the rest use unsupported algorithms or curves).

Checked against 288 OpenSSL signature vectors (96 valid, 192 altered), chains generated with OpenSSL (EC, RSA, PSS; wrong name, expired, not yet valid, forged root, missing intermediate, path length, client-only purpose, unknown critical extension, tampering). Not covered yet: name constraints and revocation (CRL/OCSP); a certificate that uses them as critical is refused. Next: the TLS 1.3 client itself.

## mouli-tls (own TLS 1.3 client, step 3)

`Connection::connect(stream, &Config)` runs a TLS 1.3 handshake over any `Read + Write` and then behaves as a stream. One cipher suite (`TLS_CHACHA20_POLY1305_SHA256`), X25519, SNI for DNS hosts. The server's chain is checked with `mouli-pki` (trusted root, validity, CA rules, host name by SAN), the CertificateVerify signature must match the leaf key (ECDSA P-256/P-384, RSA-PSS SHA-256/384), and Finished is verified. Handles tickets (ignored), KeyUpdate in both directions, `close_notify`; a close without it is reported as an error (truncation). Not supported: HelloRetryRequest, resumption, 0-RTT, client certificates, AES-GCM-only servers.

Tested against a real `openssl s_server` over loopback (skipped when openssl is absent): ECDSA and RSA servers, a 1.5 MB transfer with key updates, wrong host name, untrusted root, empty trust store, and a server that only offers AES-GCM (refused). Key-schedule values are exercised end to end by the interop; no separate published trace is checked in. Hand-written crypto: not audited for side channels.

Next: HTTP/1.1 client with streaming (step 4), then key storage and the hosted-model backend (step 5).

## mouli-http (own HTTP/1.1 client, step 4)

`Client::request(&Request)` resolves, connects (timeouts), runs `mouli-tls` for `https` with the system CA bundle (`Client::system()`), and returns the status, headers and a streaming `Body` (`Read`). One request per connection. Bodies: Content-Length, chunked (extensions and trailers accepted), or until close; a truncated body is an `UnexpectedEof` error. Strict parsing: CRLF only, no folded headers, bad or conflicting lengths and Content-Length together with chunked are refused, head size capped. Header injection in requests is refused. Plain `http` is allowed only to loopback; redirects are not followed.

Tested with canned responses (valid and malformed), a loopback server streaming chunked events, a POST echo, and HTTPS to a real `openssl s_server` through the in-house TLS (plus a wrong trust store). Next: API key storage on MOULI-DATA and the hosted-model backend (step 5).

## mouli-hosted (hosted-model client and API key storage, step 5a)

`chat_stream(&Client, &Provider, turns, max_tokens, on_token)` posts an OpenAI-compatible `/chat/completions` request with `stream: true` through `mouli-http` (own TLS, certificate checks) and calls `on_token` with each piece as it arrives; a reply that ends before the provider says it is finished is an error. Also handles servers that ignore `stream`. The key is a `Secret` (never printed, header-injection-safe, only sent in `Authorization`; error messages from the provider have it removed; plain `http` to a non-loopback provider is refused before anything is sent). `keyfile::{save,load,remove}` keep one `<provider>.key` file per provider in a directory (mode 0600, atomic replace, provider names cannot escape the directory, loose permissions reported on load).

Tested against a fake provider on loopback (header and body contents, streaming with unicode escapes, 401 without key echo, non-streaming, cut-off streams) with unit tests for the event parser. Not yet done (step 5b): wiring into `moulid` (a key entry screen, the key directory on MOULI-DATA once persistence lands, a "hosted model" choice in the picker) and routing hosted calls through the governed interface so washing and policy apply. Not tested against a real provider.

## moulid: hosted models (step 5b)

The model picker has `h = hosted model (online)`. Setup asks for the provider address (https only; http only for this computer), the model name and the API key (typing hidden), offers to save the key on MOULI-DATA (`keys/hosted.key`, mode 0600; settings `hosted.url`, `hosted.model` remembered as defaults) and requires typing `yes` to a statement that messages leave the computer, where they go, and what is and is not washed. Messages are washed with `mouli-interface::wash` (emails, card numbers, passwords and API keys replaced) before sending; the key is never printed or put in a body or URL. Replies come through `mouli-hosted` over the in-house TLS with the image's CA bundle. Turns are logged to MOULI-DATA like local chats.

Tested with scripted console input and a fake provider on loopback (consent required, key never echoed, washing of the request body, readable network error). Not done: live token display while streaming (the reply appears when complete), routing through the full `Workspace` policy and audit log, key editing/removal screen, Wi-Fi (no way to get online without a cable yet), and a test against a real provider.

## os/: Wi-Fi

Kernel: cfg80211/mac80211 with built-in drivers for Intel (iwlwifi), Realtek (rtw88/rtw89), MediaTek (mt7921) and Atheros (ath9k/ath10k) plus `mac80211_hwsim` for tests. Image: `wpa_supplicant` (WPA/WPA2 personal, no OpenSSL, AP support for the CI test), `wpa_cli`, `iw`, `wireless-regdb`, linux-firmware for those chips. `moulid::wifi`: adapter detection, rfkill unblock, scan parsing (dedupe, sort, security from flags), config generation (SSID as hex, passphrase checked, mode 0600 in /run), join with wait for `COMPLETED`, DHCP through `net::lease`, and the `w` connect screen. `mouli.wifitest=1` runs a self-test on two virtual radios (AP + client, wrong password refused, right password joins); CI boots it.

Unit-tested with a fake runner; the full path is exercised in CI only. Not verified on real radios, no WPA3/enterprise/hidden networks, no saved networks, no NTP.
