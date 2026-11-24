# MOULI OS roadmap (PR 2–6)

Each item is one pull request with acceptance criteria. Architecture: [MOULI_OS_ARCHITECTURE.md](MOULI_OS_ARCHITECTURE.md).

## PR 2 — Bootable image + moulid + llama.cpp + QEMU boot test
- `os/buildroot/` external tree → `mouli-x86_64.iso` (CPU first).
- `userspace/moulid/` C init (PID 1): mounts, hardware probe (port `theory_forge hw` to C), supervision, logs.
- `userspace/engine/llama.cpp` submodule (MIT), CPU + Vulkan; `llama-server` supervised by moulid.
- **Done when:** CI boots the ISO in QEMU, sees `MOULI ready`, gets one offline chat reply from a tiny GGUF model, and moulid prints the hardware plan. Native tests still pass.

## Engine track (E1–E5) — between PR 2 and PR 3
llama.cpp is replaced by MOULI's own C engine, targeting x86_64 laptop CPU and GPU. Plan, milestones
and acceptance criteria: [MOULI_ENGINE_PLAN.md](MOULI_ENGINE_PLAN.md). PR 3 builds on the engine.

## PR 3 — Model services: search, select, build, load
- search: Hugging Face API, GGUF/safetensors only, licence filter, header-only hardware-fit check.
- select: robust score S + cascade routing (`integrations/theory`); S shown for every model.
- build: streamed TIES merge → `convert_hf_to_gguf` → `llama-quantize`; kept only if held-out S does not drop.
- load: hardware-aware quantisation; large MoE models through `core/mouli_expert_store.c`.
- **Done when:** search → select → download → load works in the image; a merge of two tiny models yields a loadable GGUF; a tiny MoE under a RAM budget smaller than the model gives the same output as a full load.

## PR 4 — Agents (AgentBox via TOPSTAR) with sandboxing
- AgentBox integrated through TOPSTAR intent/action/result/evidence contracts; MOULIONE stays the identity/audit authority.
- Sandbox: Landlock + seccomp + namespaces, user permission prompts, file undo snapshots.
- **Done when:** a tool runs only after approval; disallowed paths/syscalls are blocked by tests; every action leaves a TOPSTAR evidence record.

## PR 5 — Chat interface (LibreOne) in a kiosk
- LibreOne UI served locally in a cage/Wayland kiosk; TTY fallback.
- Model picker with robust S, size and hardware fit; switch models without reboot.
- **Done when:** the image boots to the chat UI (or TTY), history survives reboot (SQLite), everything works offline.

## PR 6 — mouli-kernel + mouli_mem + mouli_gate + GPU
- `kernel/`: Linux LTS reference + `kernel/patches/` + x86_64/arm64 defconfigs (GPL-2.0).
- `mouli_mem` pinned huge-page device (FAST expert tier); `mouli_gate` BPF-LSM enforcement of MOULI gate policies.
- io_uring DISK tier; NVIDIA open modules and Mesa/Vulkan.
- **Done when:** the image boots on mouli-kernel in QEMU; modules load; a gate test blocks a disallowed action; on real GPU hardware llama.cpp offloads layers and the FAST tier uses GPU memory.
- **Precondition:** repository LICENSE chosen.
