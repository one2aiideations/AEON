# MOULI kernel strategy

Status: plan only. Nothing here is implemented or verified. The repository LICENSE is undecided.

## Decision

MOULI's long-term goal is its own kernel. Laptop hardware needs drivers (GPU, Wi-Fi, Bluetooth, NVMe,
USB, audio, camera, power) that only Linux has in practice. MOULI will therefore use **Linux as a
driver host**: a minimal Linux runs isolated and owns the hardware, while MOULI's own kernel runs the
MOULI system.

Reason for isolation: Linux drivers are GPL-2.0. Keeping Linux in its own protection domain, talking to
MOULI over defined interfaces, keeps MOULI's code separate from the GPL code. This is a technical
design intent, not a legal conclusion; it needs legal review before a product ships.

## Phases

| Phase | System | Purpose | Done when |
|---|---|---|---|
| K0 | Linux (Buildroot) on bare metal, MOULI userspace on top | First sellable laptop product (PR 2 to PR 6 as planned) | Roadmap PR 6 criteria |
| K1 | MOULI kernel on QEMU/virtio only: boot, memory, scheduler, IPC, ELF loader | Prove the own kernel with no hardware drivers | CI boots it in QEMU and runs moulid |
| K2 | MOULI kernel hosting a Linux driver domain (IOMMU-isolated VM) | Hardware access for GPU, network, disk, input | Linux guest boots; MOULI gets a block device and network over virtio |
| K3 | GPU path: pass the GPU (or a Vulkan-over-virtio path) from the driver domain to the engine | GPU inference and video | Engine E5 logits equal the CPU path through this path; real hardware needed to verify |
| K4 | Laptop bring-up: suspend/resume, battery, thermals, touchpad, camera, audio | Product-grade laptop | Per-model hardware test matrix |

K0 is the product path. K1 to K4 are a parallel track and do not block the product.
The design for K1 to K4 (Zircon-style Rust microkernel plus a Linux driver bridge) is in
`MOULI_MICROKERNEL_SPEC.md`.

## Design rules

- moulid and the engine talk to the system only through a small portable interface (files, sockets,
  memory mapping, device nodes), so they run on K0 or K2 without a rewrite.
- The kernel's own code (K1 onward) is written fresh. Existing kernel prototypes in sibling repos may
  be read for ideas.
- Driver-domain interfaces are plain virtio and documented, so the GPL boundary is a protocol boundary.

## Risks (stated up front)

- A hypervisor-grade kernel that safely hosts Linux with IOMMU passthrough is large work (compare
  Xen or seL4 projects). Estimate it in person-years, not months, before committing.
- GPU is the hard case: passthrough gives the driver domain full control of the GPU, so MOULI apps would
  reach it only via the Linux side; Vulkan-over-virtio exists but is slower and less mature.
  Neither is verified for MOULI.
- Real laptops differ widely; every K4 item needs hardware testing that CI cannot do.
- Legal: the GPL boundary argument must be reviewed by a lawyer.
