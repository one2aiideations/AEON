# MOULI

MOULI is a lean, AI-native operating system for x86_64 laptops (CPU and GPU), written in Rust. It boots
from a Buildroot image in which `moulid` is PID 1 and llama.cpp is the local inference engine
(`os/README.md`). Everything runs through one governed interface: policy, approvals by a second
person for consequential actions, and a hash-chained audit log.

Status: early development. Nothing here is production-ready. The desktop, the applications and the
screens below are tested in software only; they are not yet part of the boot image, which still
runs `moulid`'s own console.

## Operating system and hardware

| Crate | Purpose |
|---|---|
| `moulid` | Init (PID 1), console, networking, Wi-Fi, persistence, inference-server supervisor |
| `mouli-hw` | Hardware probe and run plan |
| `mouli-update` | Signed OS updates, A/B slots, boot counting and rollback |
| `mouli-sched` | CPU/iGPU/dGPU/NPU placement with thermal-aware migration |
| `mouli-store` | Durable system state: key-value map with a checksummed log |
| `mouli-mesh` | Device mesh: node health, fault-tolerant dispatch, file sync |
| `mouli-sdv` | Experimental software-defined-vehicle development environment (not safety-certified) |

## Models and inference

| Crate | Purpose |
|---|---|
| `mouli-gguf`, `mouli-quant`, `mouli-tokenizer` | GGUF reading and writing, dequantisation, tokenizers |
| `mouli-tensor`, `mouli-model`, `mouli-engine`, `mouli-llm` | Tensors, model index, CPU kernels, llama-family forward pass |
| `mouli-compress` | Weight analysis and compression planning |
| `mouli-hosted` | Hosted-model chat client (streaming) over MOULI's own HTTPS |
| `mouli-http`, `mouli-tls`, `mouli-pki`, `mouli-crypto` | Own HTTP/1.1 client, TLS 1.3 client, X.509 verification, crypto primitives |

## Governed interface and applications

| Crate | Purpose |
|---|---|
| `mouli-interface` | The governed workspace: sessions, intents, policy, approvals, data washing, routing, audit |
| `mouli-gateway` | Local socket protocol so other programs and agents can use the workspace |
| `mouli-auth` | Tenants, principals, deny-by-default policy, maker-checker approvals |
| `mouli-gate`, `mouli-identity` | Permission gate for AI actions; DIDs, signed frames, human-presence checks |
| `mouli-apps` | Sheet, Studio, CRM and Code as governed capabilities |
| `mouli-sheet`, `mouli-studio` | Spreadsheet core; documents (the other engines are fail-closed stubs) |
| `mouli-crm` | CRM domain: leads, pipeline with approvals, cases, renewals, consent |
| `mouli-code` | Unified-diff engine, patch policy, atomic apply and exact rollback |
| `mouli-project` | Project management: tasks, dependencies, critical path, kanban |
| `mouli-mail`, `mouli-web`, `mouli-comms` | Mail server, browser agent layer, encrypted messaging |
| `mouli-biz`, `mouli-business`, `mouli-ops`, `mouli-data`, `mouli-system` | Business, operations, data and control-centre applications |
| `mouli-security` | Detection rules, anomaly scoring, cases and evidence |

## Screens and desktop

| Crate | Purpose |
|---|---|
| `mouli-screen` | Screens as dependency-free state machines: chat, models, approvals, audit, security, review, sheet, studio, code editor |
| `mouli-desk` | Classic desktop (windows, menu bar, icons, pointer) drawn in software |
| `mouli-device` | Controller joining the screens to the governed workspace |
| `mouli-shell` | Runs the screens on a Linux framebuffer and keyboard |

## Agents, memory and research

| Crate | Purpose |
|---|---|
| `mouli-agents`, `mouli-orchestra`, `mouli-forge` | Scheduled agents, multi-agent orchestration, versioned agent manifests |
| `mouli-runtime`, `mouli-core`, `mouli-adapters`, `mouli-topstar` | Agent loop, kernel objects, TOPSTAR adapters and domain model |
| `mouli-memory`, `mouli-agentmem` | Local retrieval; agent-memory primitives (provenance, decay, replication, audit journal) |
| `mouli-cognition`, `mouli-neuro`, `mouli-theory` | Cognitive primitives, spiking runtime, theory layer |
| `mouli-lab` | Aetheron research harness with gradient checks (no real-data results yet) |

## Product specifications (no code yet)

`mouli-architecture`, `mouli-cloud`, `mouli-commerce`, `mouli-connect`, `mouli-edu`, `mouli-erp`,
`mouli-finance`, `mouli-health`, `mouli-insure`, `mouli-intelligence`, `mouli-legal`,
`mouli-logistics`, `mouli-master-prompts` and `mouli-work` are specification folders only.

## Build and test

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
os/build.sh        # the boot image (needs Buildroot and the musl target; see os/README.md)
```
