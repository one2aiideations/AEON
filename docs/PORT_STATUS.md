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
