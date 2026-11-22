# AEON language policy: Rust

Status: plan only. Nothing here is implemented or verified. This replaces the earlier rule of keeping
strict C11 flags for new code.

## Decision

All new AEON code is **Rust**. Existing C is **ported to Rust module by module** and then retired.
Reasons: memory safety for code that parses untrusted input (model files, network messages, documents,
calls), compiler-checked concurrency, and one language across engine, services, apps and the own
kernel (K1), with the TOPSTAR and yndra Rust code reusable as a reference.

## Exceptions that stay non-Rust (not ours to rewrite)

- The Linux kernel and its drivers (K0, and the K2 driver domain).
- The embedded browser engine and WebRTC stack (C/C++ upstreams), wrapped by Rust code.
- GPU vendor drivers and Mesa/Vulkan loaders.
- Test-time oracles: llama.cpp (engine tests only) and the Python theory toolkit (reference for S,
  gap law, cascade, 55D). Neither ships in the image.

## Scale of the port

About 2,900 lines of C in `core/` and about 1,200 in `userspace/aeond/`, with their tests. This is
small enough to port early, before the engine grows on top of it. (Line counts measured with `wc`.)

## Migration rules

1. **The C code stays until its Rust replacement matches.** Each module is ported with a
   differential test: the same inputs go through the C version (called through the C interface) and
   the Rust version, and the outputs must be identical. Only then is the C module removed, and
   removal is confirmed with the owner before any file is deleted.
2. **Order.** (a) Cargo workspace and CI; (b) `aeon-hw` and aeond, because the image needs them and
   they are small; (c) `core/` modules, bottom-up (tensor, buffer, storage, cache, expert store,
   transformer compute, then the gate and identity modules); (d) the engine E1+ written in Rust from
   the start.
3. **PR #2 ships as it is** (C aeond, llama.cpp stand-in). The Rust aeond replaces it in a later PR.
4. **`unsafe` is confined** to named modules (SIMD kernels, mmap, FFI to C libraries and the kernel).
   Every `unsafe` block carries a comment stating its invariant.
5. **No third-party crate without review.** Dependencies are pinned, audited with `cargo-deny`, and
   kept small; the image stays lean.

## Toolchain and CI

- Cargo workspace; `edition = 2021` or newer; `#![forbid(unsafe_code)]` by default per crate.
- CI gates: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, Miri on crates with
  `unsafe`, `cargo-deny` and `cargo-audit`, plus fuzzing (`cargo-fuzz`) for every parser (GGUF,
  wire formats, tokenizer).
- Image: Buildroot Rust/cargo package infrastructure, static musl binaries where possible,
  `panic = "abort"`, LTO, size-optimised release profile. The image size and boot time budgets from
  the lean-OS plan are checked in CI.
- Sandboxing (Landlock, seccomp, namespaces) still applies per service process; Rust does not
  replace it.

## Effect on other plans

- Engine milestones E1–E6: written in Rust (`aeon-gguf`, `aeon-tokenizer`, `aeon-engine`,
  `aeon-vulkan`); SIMD via `core::arch` behind runtime detection.
- Model services (PR 3), agents (PR 4), chat UI glue (PR 5): Rust.
- Kernel track K1: Rust, `no_std`.
- "Strict C11 flags, ASan/UBSan" applies only while C code remains.

## Risks (stated up front)

- Rust is not a substitute for review or tests: logic and protocol errors, and mistakes inside
  `unsafe`, remain possible.
- Compile times and image build time grow.
- The Rust toolchain and WSL2 are not installed on the current Windows development machine; builds are
  verified in CI until a local toolchain is set up.
- The port delays E1 by the time needed for steps (a) and (b).
