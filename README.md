# AEON

AEON is a lean, AI-native operating system for x86_64 laptops (CPU and GPU), written in Rust.

## Crates

| Crate | Purpose |
|---|---|
| `aeon-hw` | Hardware probe and run plan |
| `aeon-gate` | Permission gate for user and AI-initiated actions |
| `aeon-gguf` | GGUF model file types (reader in progress) |
| `aeon-tensor` | Tensor layouts, views, bump arena |
| `aeon-model` | Model index, manifest, bounded reads, tiered MoE experts |
| `aeon-engine` | Inference engine (CPU reference kernels so far) |
| `aeon-identity` | DID registry, signed semantic frames, human-presence checks, I/O gates |
| `aeon-cognition` | Hypervectors, spiking net, active inference, causal graph, memory, workspace |
| `aeon-runtime` | Agent loop, capability-checked tools, gated actions |
| `aeon-adapters` | Adapters to sibling systems (TOPSTAR) |
| `aeond` | Init (PID 1) and inference-service supervisor |
| `aeon-theory` | Robust score S, gap law, calibration, cascades, TIES merging |

## Build and test

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Status: early development. Nothing here is production-ready.
