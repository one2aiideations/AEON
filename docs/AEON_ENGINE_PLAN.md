# AEON engine plan (replacing llama.cpp)

Status: plan only. Nothing in this document is implemented or verified yet.

## Decision

AEON will run models with its own inference engine, written in C11 in this repository, instead of
llama.cpp. llama.cpp (MIT) stays in the image only until milestone E4. It is also used, at test time
only, as a correctness oracle. It never ships in the final image.

Reasons: one codebase under One2AI's own licence terms, tight integration with `aeon_expert_store.c`
and the robust-S model services, and control over memory use on laptops.

Linux (GPL-2.0) remains the kernel. GPL does not prevent selling a product, but kernel patches and
modules must be GPL-2.0 and their source offered. The repository LICENSE is undecided and is a
decision for the owner.

## First product target

- Architecture: **x86_64 only** (Intel and AMD). arm64, Apple Silicon and Snapdragon are out of scope.
- Hardware: **laptop CPU and laptop GPU** (integrated and discrete, Intel/AMD/NVIDIA).
- Models: 1–8B parameters, 4–6 bit quantised (about 1–5 GB), dense first, MoE through
  `core/aeon_expert_store.c`.
- Offline operation.

Consequences:
- Q4_K, Q5_K, Q6_K and Q8_0 are required formats; token speed is bound by memory bandwidth, so every
  milestone reports tokens/s on a named machine.
- Vulkan is the primary GPU backend because it covers all three vendors. CUDA is optional and only
  built if Vulkan on NVIDIA benchmarks poorly.
- Integrated GPUs share system RAM; aeond's hardware plan must split memory between weights, KV cache
  and GPU offload.
- Battery/thermal limits, suspend/resume and arbitrary laptop drivers are risks owned by PR 6.

## Milestones

Each is one stacked pull request with tests.

| # | Milestone | Done when |
|---|---|---|
| 0 | PR 2: image with llama.cpp as stand-in | CI boot log shows `AEON ready`, `AEON chat reply:`, `AEON selftest: PASS` |
| E1 | GGUF reader; BPE/SentencePiece tokenizer; F32/F16/BF16/Q8_0/Q4_0/Q4_K/Q5_K/Q6_K dequant | Metadata, tokens and tensor values equal the oracle on tiny GGUFs |
| E2 | CPU forward pass: RMSNorm, RoPE, GQA, SwiGLU, KV cache, sampler | Logits within stated tolerance of the oracle; greedy output identical on tiny models |
| E3 | SIMD kernels (AVX2, AVX-512; runtime detection) and threading | Match the portable path; benchmark reported |
| E4 | HTTP server with aeond's current interface; MoE via expert store; remove llama.cpp from the image | QEMU boot test passes with no llama.cpp in the image |
| E5 | Vulkan compute backend with hardware-aware layer offload | Logits equal the CPU path on a software Vulkan driver (lavapipe) in CI |
| 3 | PR 3 model services (see roadmap) on top of the engine | Roadmap PR 3 criteria |
| E6 | CUDA backend (optional) | Needs real NVIDIA hardware |

## Rules

- Write from the GGUF specification and published papers. Do not copy llama.cpp source.
- Strict flags `-std=c11 -Wall -Wextra -Wpedantic -Werror`; a test for every module; ASan/UBSan on new C.
- The oracle is downloaded and built only in CI test jobs, never committed or shipped.
- No model weights, caches or secrets in the repository.

## Verification limits (stated up front)

- CI runners have no GPU. Vulkan correctness is checked on lavapipe (software), not speed.
- Real-GPU behaviour and laptop speed are unverified until run on actual hardware and reported with
  the machine and numbers.
- A from-scratch engine will be slower than llama.cpp until E3 and E5 land.
