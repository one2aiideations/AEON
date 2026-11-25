# MOULI microkernel specification (K1 to K4)

Status: **specification only. Nothing here is implemented or verified.** The product stays on **K0**
(Linux built with Buildroot, `moulid` as PID 1; see `MOULI_KERNEL_STRATEGY.md`). This document defines the
future own kernel so the work can start later without re-deciding the design.

MOULI's own code is free for distribution. Existing kernels (Zircon, seL4, Redox, Asterinas, Genode) are
**inspiration only**: no code is copied, and no third-party crate is added without review.

## 1. Goals and non-goals

Goals
- A small microkernel written in Rust, in the style of Zircon: kernel objects, handles that carry rights,
  no ambient authority, drivers and services in user space.
- Linux runs as an isolated **driver domain** and provides real hardware drivers over a documented bridge.
- `moulid` and the engine run unchanged on K0 and on this kernel (portable interface rule, strategy doc).
- Capability checks in the kernel line up with the MOULI-AUTH policy model.

Non-goals
- Writing hardware drivers for GPU, Wi-Fi, Bluetooth, NVMe or audio in the kernel (Linux provides them).
- POSIX compatibility inside the kernel. POSIX is a user-space library over kernel objects.
- Formal verification (not required; a later option, see seL4 for what that costs).
- Replacing K0 before K2 passes on real hardware.

## 2. Architecture

```
 +---------------------------------------------------------------+
 |  Apps: moulid, engine (llama.cpp), MOULI-AUTH, desktop, apps   |  user mode
 +----------------+----------------------+-----------------------+
 |  Services: vfs, net, display, device manager (all user mode)   |
 +----------------+----------------------+-----------------------+
 |  Bridge broker  <==== virtio + shared memory ====>  Linux       |  driver domain
 |  (user mode)                                      (VM, IOMMU)  |  (GPL-2.0)
 +---------------------------------------------------------------+
 |  MOULI microkernel: objects, handles, scheduler, memory, IPC,  |  kernel mode
 |  interrupts, timers, virtualisation (VMX/SVM)                  |
 +---------------------------------------------------------------+
```

The kernel does only: address spaces, threads and scheduling, memory objects, IPC, interrupt delivery,
time, and running a guest VM. Everything else is a user-mode component holding handles.

## 3. Kernel objects (Zircon style)

| Object | Purpose | Notes |
|---|---|---|
| Process | Address space plus a handle table | Created with an empty table; receives handles only by transfer |
| Thread | Unit of execution | Priority and affinity set through a scheduler handle |
| Vmo | Virtual memory object (pages) | Mapped into address spaces; copy-on-write clones; can be pinned for DMA |
| Vmar | Address-space region | Controls where a Vmo may be mapped |
| Channel | Bidirectional message pipe | Carries bytes and handles; the basic IPC |
| Port | Wait set | Receives signals from many objects |
| Event, Eventpair | Signalling | Signals are bits on the object |
| Interrupt | A hardware interrupt line | Only the holder can wait or acknowledge |
| Timer | One-shot or periodic | |
| Iommu / Bti | DMA address translation and pinning | Confines a device or a guest to granted memory |
| Guest, Vcpu | Virtual machine and CPU | Hosts the Linux driver domain |
| Job | Group of processes | Policy and resource limits, kill together |
| Resource | Right to create hardware-facing objects | Root held only by the device manager |

Rules
1. A **handle** = (object reference, rights bitmask). The kernel checks rights on every call.
2. Handles can be duplicated or transferred with equal or fewer rights, never more.
3. There is no global name space and no "root" bypass in the kernel. A process can touch only what it holds.
4. Every object has a koid (kernel object id, 64 bit, never reused) and a creating job, for accounting.
5. Resource limits (memory pages, handles, threads) are per job and enforced at creation.

Mapping to MOULI's existing model
- `mouli-core` `KernelObject` and `CapabilityRegistry` describe **AI-layer** objects (users, agents, models,
  capabilities). They stay in user space. A MOULI capability grants the *right to ask a service* that holds
  the kernel handle; the kernel does not know about agents or models.
- MOULI-AUTH issues the policy that the device manager and services apply when they hand out handles.
- The kernel audit log (section 9) feeds the same hash-chained audit as `mouli-interface`.

## 4. System calls

About 60 calls at K1, one per object operation (create, duplicate, replace, close, signal, wait, read,
write, map, unmap, and so on). Rules:
- Arguments are validated in the kernel; user pointers are accessed only through checked copy helpers.
- Every call returns a status code from one enum; no call blocks without a timeout parameter or a wait on a
  handle that can be cancelled.
- The call table is versioned. User code calls through a kernel-provided **vDSO-style page** so the numeric
  interface can change without rebuilding programs.

## 5. Implementation language and safety

- Rust 2021, `no_std`, with `core` and `alloc` only. Target `x86_64` first, then `aarch64`.
- `unsafe` is allowed **only** inside a small `kernel/arch` and `kernel/mem` core (page tables, context
  switch, port and MMIO access, VMX/SVM). Each `unsafe` block carries a `// SAFETY:` comment stating the
  invariant. The rest of the kernel is `#![forbid(unsafe_code)]` (idea taken from the Asterinas framekernel).
- Goal: the unsafe core stays under 5 percent of kernel lines; CI prints the real figure.
- No third-party crates in the kernel. Bootloader is the one open question (section 12).

## 6. Memory

- Physical memory manager: bitmap or buddy allocator, per-zone, with a guard for DMA-reachable ranges.
- Virtual memory: 4-level paging, W^X enforced, kernel and user separated, guard pages on stacks.
- Vmo-based: no `fork`; processes are built by mapping Vmos.
- Pinning for DMA only through a Bti bound to an Iommu; without an IOMMU the device manager refuses
  to hand out DMA-capable handles for untrusted domains.

## 7. Scheduling and time

- Fixed-priority classes with round-robin inside a class; later a deadline class for inference and audio.
- Per-CPU run queues, wake-up IPIs, tickless idle at K4.
- Thermal and placement hints from `mouli-sched` arrive as scheduler-handle calls from user space.
- Monotonic clock from TSC or HPET, calibrated at boot; wall clock supplied by a user-mode time service
  (the SNTP code in `moulid` is reused).

## 8. The Linux driver bridge (K2)

Purpose: real hardware, GPU included, without writing drivers.

Design
- A minimal Linux kernel plus a small user space runs inside a **Guest** with an IOMMU-confined view of
  the devices the device manager assigns to it (PCI passthrough for NVMe, Wi-Fi, GPU, USB, audio).
- The bridge broker (user mode, MOULI-owned) talks to the guest only over **virtio** and shared memory:

| Service to MOULI | Mechanism | Notes |
|---|---|---|
| Block storage | virtio-blk | MOULI-DATA lives here |
| Network | virtio-net | Linux owns Wi-Fi and firmware; MOULI sees an Ethernet-like link |
| Input | virtio-input | keyboard, touchpad |
| Display | virtio-gpu (2D first) | Vulkan-over-virtio later (K3) |
| Audio | virtio-snd | |
| Power and battery | small MOULI-defined virtio-style channel | suspend, battery, thermal |
| Console and control | virtio-vsock | for management; authenticated by MOULI-AUTH |

- Linux is treated as **untrusted for MOULI's data**: it sees only what it is handed. Anything stored
  through it is encrypted or integrity-checked by MOULI first (MOULI-DATA keys never enter the guest).
- Passed-through devices are confined by the IOMMU; the guest cannot DMA outside its granted ranges.
- The bridge protocol is plain, documented virtio, so the **GPL boundary is a protocol boundary**. This is a
  design intent, not a legal conclusion; a lawyer must review it before shipping (strategy doc).
- Crash handling: if the guest dies or hangs, the broker restarts it; services see I/O errors and retry.
  MOULI's own services keep running.

Known hard cases (unsolved at spec time)
- GPU passthrough gives the guest full GPU control; MOULI apps reach the GPU only through the guest.
- Suspend and resume must be coordinated between kernel, broker and guest.
- Boot time and memory cost of a guest (target to measure: guest under 256 MB resident).

## 9. Security model

- Capability = handle with rights; enforced by the kernel. No ambient authority, no superuser.
- Components start with an empty handle table; the device manager and a launcher build each tables from
  a manifest signed through MOULI-AUTH.
- Kernel audit: handle creation of Resource and Guest objects, policy-relevant failures and process
  start/stop are written to a bounded ring, drained by a user-mode service into the hash-chained audit.
- Secrets: keys live in a vault service; the kernel only provides memory isolation and zeroing on free.
- Side channels, speculative execution and firmware trust are acknowledged risks and are not claimed solved.
- Secure Boot and measured boot are K4 items.

## 10. Phases and exit criteria

| Phase | Deliverable | Exit test (all in CI under QEMU unless stated) |
|---|---|---|
| K1a | Boot to Rust `kmain` on x86_64, serial log, page tables, interrupts, timer | Boots under BIOS and UEFI, prints banner and tick count |
| K1b | Processes, threads, scheduler, Vmo, Channel, Port, handle rights | Unit tests for rights (cannot escalate), and an IPC ping-pong test in QEMU |
| K1c | ELF loader, user-mode init, a first service, `moulid` core logic running as a user process | `moulid` self-test prints its banner from user mode; mutation test: removing a rights check fails a test |
| K2a | Guest and Vcpu on VMX, boot a minimal Linux in a guest | Guest prints its kernel version over vsock |
| K2b | virtio-blk, virtio-net, virtio-input brokered | MOULI-DATA read and written through the guest; DHCP and a download through virtio-net |
| K3 | GPU path (passthrough or Vulkan-over-virtio) | Engine logits equal the CPU path (existing criterion); **needs real hardware** |
| K4 | Laptop bring-up | Per-model hardware test matrix; suspend and resume; battery; thermal |

K1 and K2a/b can be completed and verified in CI. K3 and K4 need real machines and cannot be claimed
from CI alone.

## 11. Testing

- Host-side unit tests for all logic that does not need hardware (rights algebra, allocators, scheduler
  queues, message formats), run with plain `cargo test`.
- QEMU boot tests in CI as with `os-image.yml`: banner grep, self-test, timeouts.
- Property tests for the handle table: duplication never raises rights; closing is idempotent-safe;
  koids never repeat.
- Fault injection: kill the Linux guest during I/O, exhaust handles, exhaust pages, send oversized
  messages; each must end in an error status, never a kernel panic.
- Mutation checks on every rights check, as done for `mouli-player`.
- A kernel panic is a bug; every panic path is listed and justified.

## 12. Open questions (decide before K1a)

1. Bootloader: write a small UEFI stub or use an existing one (review licence and size).
2. Allocator and scheduler algorithms: start simple; replace once measurements exist.
3. Hypervisor scope: VMX only first, or also AMD SVM and ARM virtualisation at the same time.
4. How `moulid` is split: which parts become services, which stay one process (the portable interface rule
   says files, sockets, memory mapping and device nodes; those become user-mode libraries here).
5. Whether the Linux guest uses the same kernel build as K0 (yes proposed, to share the driver testing).
6. Licence of MOULI's own kernel code: free for distribution, formal licence undecided.
7. Staffing and time: not estimated here. Estimate K1 after K1a is working; compare Zircon, seL4 and
   Redox timelines before committing to K2 and K3.

## 13. Risks

- Hypervisor-grade isolation with IOMMU passthrough is large work and easy to get subtly wrong.
- GPU remains the hardest part and may force the Linux side to own the display.
- Real laptops differ widely; CI cannot test them.
- The GPL-boundary argument needs legal review.
- A second kernel doubles the maintenance and security surface; K0 must keep shipping in parallel.

## 14. References for ideas only

Zircon (objects, handles, rights, drivers in user space), seL4 (capabilities, small trusted base),
Asterinas (small unsafe core), Genode (reusing Linux drivers in isolated components), Redox (a Rust
microkernel), NetBSD rump kernels (drivers in user space). No code is taken from any of them.
