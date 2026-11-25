# MOULI AI-native OS — architecture and plan

Status: design proposal plus the first code bridge (`core/aeon_expert_store.c`). The boundaries in
[EIGHT_SYSTEM_INTEGRATION_ARCHITECTURE.md](EIGHT_SYSTEM_INTEGRATION_ARCHITECTURE.md) still hold:
**MOULI executes**, LibreOne owns the conversational UX, AgentBox owns agents, MOULI-AUTH owns identity
and audit, TOPSTAR is the cross-system protocol, and AetherLang owns compiler lowering.

## 1. Goal

A bootable operating system whose main job is running AI locally. It provides a chat interface,
model search, model selection, on-the-fly model building, hardware-aware model loading
(including models larger than RAM) and sandboxed agents, with no cloud dependency for normal use.

## 2. Kernel decision

MOULI's product kernel is **mouli-kernel, a fork of the latest Linux LTS**, trimmed and extended
for AI workloads. Writing GPU, NVMe, USB and network drivers from scratch would take years; Linux
already provides them, and Android and ChromeOS are built the same way.

- **Licence:** kernel code, including MOULI kernel modules, is **GPL-2.0** because it is derived from
  Linux. MOULI userspace (this repository's `core/`, runtime and services) keeps its own licence.
  The system-call boundary separates the two. **The repository currently has no LICENSE file; the
  owners must choose one** (MIT or Apache-2.0 keep it open and commercially usable; proprietary is
  also possible for userspace).
- **Configs:** `aeon_x86_64_defconfig` and `aeon_arm64_defconfig`, covering NVMe, USB, net, virtio,
  DRM (amdgpu, i915), NVIDIA open kernel modules (out of tree), BPF LSM, io_uring, Landlock, seccomp,
  cgroup v2 and huge pages.
- **MOULI kernel modules:**
  - `aeon_mem`: pinned huge-page memory for model weights and the FAST expert tier.
  - `aeon_gate`: BPF-LSM programs enforcing MOULI's existing process, network and I/O gate policies
    (`core/aeon_process_gate.c`, `aeon_net_policy.c`, `aeon_io_gate.c`). This turns today's
    host-testable boundaries into real enforcement.
- `experimental/aeon_x86_kernel.c` (IDT/APIC/paging) remains research for a possible native kernel
  later. It is not on the product path.

## 3. System layout

```
┌──────────────────────── user ────────────────────────┐
│  Chat UI (LibreOne, in a Wayland kiosk) · TTY fallback │
├───────────────────── MOULI services ───────────────────┤
│ model-search │ model-select │ model-build │ model-load │ agents (AgentBox, sandboxed)
│   (Hub API)  │  (robust S)  │ (TIES soup) │ (tiers)    │ Landlock + seccomp + namespaces
├───────────────────── MOULI runtime ────────────────────┤
│ inference engine: llama.cpp/ggml (MIT) + MOULI adapter  │
│ core/: tensor cache, model stream/index, expert store, │
│        transformer compute, semantic/identity gates    │
├────────────── moulid (PID 1, C) ───────────────────────┤
│ mounts, hardware probe, service supervision, updates   │
├────────────── mouli-kernel (Linux LTS fork) ───────────┤
│ drivers · aeon_mem · aeon_gate (BPF-LSM) · io_uring    │
└────────────────────────────────────────────────────────┘
```

## 4. Feature map: what exists vs what is to be written

| Feature | Exists today | To build |
|---|---|---|
| Boot image | — | Buildroot external tree → `mouli-x86_64.iso`; QEMU boot test in CI |
| Init | — | `userspace/aeond` (C): mounts, hardware probe, supervision |
| Hardware awareness | `integrations/theory` `hw` (Python) | Port to C inside moulid (VRAM, RAM, SIMD, disk → device, precision, budgets) |
| Inference | `core/aeon_transformer_compute.c` (primitives), `experimental/aeon_transformer_graph.c` | llama.cpp submodule as the engine; MOULI adapter for model loading |
| Large-model loading | **`core/aeon_expert_store.c` (new, tested)**; Python reference `sparse_runtime.py` (matches transformers exactly) | Wire the expert store into the engine's MoE path; FAST tier on `aeon_mem` |
| Model storage/streaming | `aeon_model_stream`, `aeon_model_index`, `aeon_model_manifest`, NVMe queue reads, tensor cache with budget and eviction | Back the DISK tier with io_uring reads |
| Model search | `forge` discovery + header-only safetensors checks | C/HTTP service, GGUF/safetensors only, licence filter |
| Model selection | robust S, cascade routing (`theory_layer.py`) | Service API; S shown for every listed model |
| On-the-fly model building | `merge` / `forge` (streamed TIES, kept only if S does not drop) | Service: merge → `convert_hf_to_gguf` → `llama-quantize` |
| Agents | AgentBox (separate repo) | Integration through TOPSTAR contracts; sandbox with Landlock, seccomp and namespaces; user permission prompts |
| Chat UI | LibreOne (separate repo) | Local web UI in a cage/Wayland kiosk |
| Security | identity, semantic, process, network and I/O gates (host-tested) | Real enforcement via `aeon_gate` BPF-LSM |

## 5. Delivery plan (one pull request each)

1. **This PR:** theory integration, expert store (C), bounded attention, this document.
2. Buildroot image + moulid + llama.cpp submodule + QEMU boot test (CPU only).
3. Model services: search, select, build, load (hardware-aware quantisation).
4. Agents + sandbox (AgentBox via TOPSTAR).
5. Chat UI kiosk (LibreOne).
6. Kernel: mouli-kernel configs, `aeon_mem`, `aeon_gate`, GPU support.

## 6. Verified vs not verified

- **Verified (host tests):** all native tests, including the expert store. It serves experts from
  FAST, RAM and disk tiers, evicts by LRU, keeps pinned experts, honours allow masks, and computes a
  MoE forward pass through a 1-slot cache that matches a dense reference to within 1e-6 (also clean
  under ASan/UBSan). Attention handles a 4096-token context using caller scratch memory.
- **Not verified:** booting on hardware, the kernel, GPU execution, the inference engine adapter,
  and real-model performance. Nothing here claims MOULI is a finished OS.
