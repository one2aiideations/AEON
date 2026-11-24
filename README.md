# MOULI

MOULI is a lean, AI-native operating system for x86_64 laptops (CPU and GPU), written in Rust.

## Crates

| Crate | Purpose |
|---|---|
| `mouli-hw` | Hardware probe and run plan |
| `mouli-gate` | Permission gate for user and AI-initiated actions |
| `mouli-gguf` | GGUF model file types (reader in progress) |
| `mouli-tensor` | Tensor layouts, views, bump arena |
| `mouli-model` | Model index, manifest, bounded reads, tiered MoE experts |
| `mouli-engine` | Inference engine (CPU reference kernels so far) |
| `mouli-identity` | DID registry, signed semantic frames, human-presence checks, I/O gates |
| `mouli-cognition` | Hypervectors, spiking net, active inference, causal graph, memory, workspace |
| `mouli-runtime` | Agent loop, capability-checked tools, gated actions |
| `mouli-adapters` | Adapters to sibling systems (TOPSTAR) |
| `moulid` | Init (PID 1) and inference-service supervisor |
| `mouli-theory` | Robust score S, gap law, calibration, cascades, TIES merging |
| `mouli-agents` | Scheduled agents, runner, hash-chained audit log |
| `mouli-sched` | CPU/iGPU/dGPU/NPU placement with thermal-aware migration |
| `mouli-core` | Kernel objects, capability registry, cognitive engines, agent lifecycle |
| `mouli-neuro` | Event-driven spiking runtime with local learning |
| `mouli-mesh` | Device mesh: node health, fault-tolerant dispatch, chunked file sync |
| `mouli-memory` | Local retrieval: BM25, vectors, knowledge graph, rank fusion |
| `mouli-web` | Browser agent layer: readable text, page intents, consent gate, rate limits |
| `mouli-comms` | Encrypted messaging, ordered conversations, call signalling |
| `mouli-store` | Durable system state: in-memory map with checksummed log |
| `mouli-orchestra` | Multi-agent orchestration: trust, routing, consensus, governance, resilience, self-repair |
| `mouli-mail` | AI-native mail server: SMTP submission, minimal IMAP, HTTP API, agent permissions |
| `mouli-sheet` | Spreadsheet core: formulas, dependency-ordered recalculation, cycle detection |
| `mouli-studio` | One creative/office app: documents, spreadsheets (mouli-sheet), fail-closed engine stubs |
| `mouli-project` | Project management: tasks, dependencies, critical path, kanban, REST API, optional desktop UI |

## Build and test

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Status: early development. Nothing here is production-ready.
