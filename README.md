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

## Build and test

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Status: early development. Nothing here is production-ready.
