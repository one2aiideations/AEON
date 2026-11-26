# MOULI-LLM: one engine, many ways to get the weights into memory

Status: **design only.** What exists today is listed in section 2; everything else is a plan, with the evidence for each
idea marked. Nothing in sections 3 to 10 is implemented or verified. Supersedes the C-era `docs/MOULI_ENGINE_PLAN.md`
(its milestone names E1 to E6 are kept where they still apply).

## 1. Purpose and non-goals

Purpose: run language models, small and large, on whatever machine MOULI is on, without loading more of the model into RAM
than the job needs, and tell the person honestly what to expect (speed, quality risk, memory). One engine, one API, several
**strategies** for supplying weights, chosen by a **planner** from the hardware and the model.

Non-goals (for now): training; GPU backends (a later milestone); every model architecture (the list grows by tests);
beating llama.cpp's speed (the goal is to match it closely enough on target machines, or to do things it does not do);
claiming that a dense 70B model is interactive on a laptop (it is not; section 4 says what is possible).

## 2. What exists today (facts, 2026-11-25)

The former crates `mouli-gguf`, `mouli-quant`, `mouli-tokenizer`, `mouli-tensor`, `mouli-model` and `mouli-engine` were merged into this crate
(history kept) as the modules `gguf`, `quant`, `tokenizer`, `tensor`, `weights` and `kernels`. All 144 unit tests and the integration tests moved with them
and pass; `moulid` and `mouli-runtime` now depend on `mouli-llm`.
The crate is now **one source file**, `src/lib.rs` (about 8,300 lines), with the modules as inline `mod name { ... }` blocks; public paths are unchanged
(`mouli_llm::gguf::...`). Integration tests (`tests/`) and the fuzz target (`fuzz/`) stay as separate files because they are separate build targets.

| Piece | Where | State |
|---|---|---|
| GGUF reader and writer | `mouli-llm::gguf` (2,478 lines) | Round trips, malformed input and fuzz tests; not run on real model files |
| Dequantisation | `mouli-llm::quant` | Q4_0, Q8_0, K-quants, floats from the published layouts; tested on hand-built blocks; no comparison on real tensors |
| Tokenizer | `mouli-llm::tokenizer` | BPE and SentencePiece; only the GPT-2 pre-tokenizer, so other model families may split differently |
| Forward pass | `mouli-llm` (857 lines, 8 tests) | `llama` and `qwen2` dense only; scalar, single-threaded; compared with a naive implementation on tiny synthetic models; **no real-model logits comparison** |
| Reference kernels | `mouli-llm::kernels` | ops and quant helpers (about 320 lines) |
| Weight access | `mouli-llm::weights` (1,756 lines) | tensor index, one-tensor readers, byte sources (file, block device), bounded stream, tensor LRU cache, tiered MoE expert store with hot-set masks; **not used by `mouli-llm`** |
| Domain slicing logic | `sparse_runtime.py` (Python, 500 lines) | profiling, hot sets, neuron slicing, robust-score check; verified on tiny random models only |
| Mesh | `mouli-mesh` (913 lines, 18 tests) | peer network pieces; not connected to inference |
| Shipping engine | llama.cpp in the OS image | does all inference today |

## 3. Architecture

```
 request (chat / completion / batch job)            moulid, desktop, apps, LAN clients
          |
   [ API ]  OpenAI-style /v1/chat/completions (SSE streaming, cancel) + llama-server-compatible /health /completion
          |
   [ Scheduler ]  sessions, continuous batching, speculative decoding, prompt/KV cache, priorities, budgets
          |
   [ Planner ]  hardware probe + model facts + measured storage speed  ->  an ExecutionPlan and a plain-language explanation
          |
   [ Engine ]  forward pass over layers: attention (GQA, RoPE, KV cache), MLP or MoE, norms, sampler
          |            kernels: scalar (reference) | AVX2 | AVX-512 | (later) GPU
   [ WeightProvider ]  fetch(layer, tensor) and hint_next(layer)   <- the one abstraction all strategies implement
          |
   [ Format ]  GGUF reader, tokenizer, quant layouts          sources: file, mmap, block device, network peer
```

`WeightProvider` returns a read-only view of a tensor's bytes (or a lease that the engine drops after use). Providers must be
bit-identical in what they return, so every strategy can be tested against the plain resident one (section 8).

## 4. Strategies (what the planner chooses between)

Evidence levels: **S** shown in this repo's tests, **R** reproducible elsewhere (cited in the research report), **P** paper claim only, **I** my inference.

| # | Strategy | What it does | Works for | Expected effect | Evidence |
|---|---|---|---|---|---|
| 1 | **Resident** | Model fully in RAM | Anything that fits | Best speed; baseline | S for tiny models |
| 2 | **Lazy map** | Memory-map the file; pages are read when first touched; sequential pre-read on slow media | Anything that fits; MoE with unused experts | Faster first token on fast disks; slow on cold flash unless pre-read | R (llama.cpp) |
| 3 | **MoE paging** | Hot experts pinned, warm in an LRU RAM tier, cold on disk, read one at a time when routed | Mixture-of-experts models | Big-model quality at active-parameter speed if the working set fits | S for the store logic (tiny), R for MoE speed (research report) |
| 4 | **Layer streaming with read-ahead** | One or two layers in RAM; the next is read while the current computes | Any dense model, **large batches only** | RAM need drops to a few GB; speed is disk bandwidth divided by model size per pass, shared by the batch | I; P (FlexGen) |
| 5 | **Domain slicing** | Offline: profile the model on domain prompts, keep the most-used neurons per layer (and layers if safe), write a smaller GGUF, accept only if the robust score stays within tolerance | A chosen domain | Smaller file and RAM; quality loss must be measured | S for the logic (tiny), P for quality at scale |
| 6 | **Activation-sparse reads** | Skip weight columns for inactive neurons per token (thresholded), weights stored column-wise | Dense models, maybe | Less data read per token; reported mostly on GPU | P (TEAL, PowerInfer, LLM in a flash) |
| 7 | **Mesh pipeline** | Split layers across peers; each token passes through the chain | Several machines on a LAN | Holds a model larger than any one machine; latency per hop | P/R (llama.cpp RPC, prima.cpp); I |
| 8 | **Speculative decoding** | A small resident model drafts tokens, the big one verifies several per pass | Any, best with streaming or mesh | More tokens per pass over the weights | R on GPU, unmeasured on CPU |
| 9 | **Cascade** | A small model answers; the big one loads only when a router or the user escalates, then unloads | Product level | Big-model quality only when needed | I |
| 10 | **Retrieval-first** | Answer from the person's documents with a small model | Document work | Avoids needing big weights | I (separate feature, MOULI-STUDIO / document chat) |

Honest limits: a dense model needs every weight for every token, so strategy 4 turns a 70B model into batch work (about 14 s per pass from a
3 GB/s drive; with 64 conversations in one pass about 4.5 tokens/s in total, by my arithmetic, and the KV cache then dominates memory).
Strategies 3, 5, 7 and 9 are where "load only what is required" is real for interactive use.

## 5. The planner

Inputs: RAM total and free; CPU features and cores; memory bandwidth (measured by a short test, not assumed); storage type and **measured**
sequential and random read speed of the device holding the model; GPU presence; peers on the LAN and their free RAM; the model's size, layers,
experts, quantisation; the job (interactive, batch, background); the person's choice (private only, allow hosted).

Output: an `ExecutionPlan { strategy, threads, context, kv_type, prefetch, batch, expected_tokens_per_s, expected_load_time, memory_use, warnings }`
and one plain sentence ("This model needs 46 GB and this machine has 16 GB free, so it would run at about 0.1 words per second.
Use the 14B model, or run it as an overnight batch."). The planner may refuse a plan that would thrash.

## 6. Loading pipeline

Measured sequential pre-read on slow media; progress by bytes read; load time adaptive to the measured speed (no fixed timeout);
shard support for split GGUF files; checksum check before first use when known; a **load report** (bytes read, time, memory, strategy) kept
with the session; never silently fall back to a slower strategy without telling the person.

## 7. API and chat

OpenAI-compatible chat completions with **streaming (SSE)**, cancel, usage and timing in the response, plus the llama-server-compatible
endpoints `moulid` uses today, so the engine can replace llama.cpp behind a flag and be A/B tested. Optional LAN serve mode with authentication
(the "model box"). All input bounded; no unbounded allocations from request data.

## 8. Verification (the rules that keep this honest)

1. **Oracle:** llama.cpp, built only in CI test jobs, never shipped. Compare logits on real small models (Qwen2.5-0.5B and 1.5B Q4_K_M, a Llama 3.2 1B)
   within a stated tolerance, and greedy output for N tokens.
2. **Strategy equivalence:** every strategy must return the same logits as Resident for the same model and input (bit-identical where the kernels
   are the same, within tolerance otherwise). This is the main test for strategies 2 to 8.
3. Fault injection: short reads, read errors at every offset, slow disks, evicted pages, killed peers; each must end in an error or a retry, never wrong output.
4. Memory bound tests: resident bytes never exceed the plan's budget (measured, not assumed).
5. Benchmarks are reported with the machine, model, quantisation, threads and warm or cold cache. No speed claim without one.
6. Slicing gate: a sliced model ships only with its robust-score report next to the full model's, on held-out data; the mean alone is not accepted.
7. Mesh: authenticated, encrypted transport; a peer cannot make the engine allocate unbounded memory or return silent garbage.

## 9. `unsafe` policy

The workspace forbids `unsafe`. Two places need it: SIMD intrinsics (`std::arch`) and memory mapping. Rule: each lives in one small module with a
documented `// SAFETY:` comment per block, a scalar reference implementation, and a test that compares the two on random inputs including edge sizes.
No other module may use `unsafe`. The memory-mapping crate already in the workspace may be used; any new dependency needs the owner's review.

## 10. Milestones (each a separate change, each with tests and a stated exit)

| # | Milestone | Exit criterion |
|---|---|---|
| M0 | **Decision gate: measure.** Run a real 70B Q4 file with llama.cpp in a VM limited to 8 GB RAM; record tokens/s and I/O. Also run a real MoE model. | Numbers recorded; decide how much of 4 to 7 is worth building |
| M1 | Real-model parity (E2): tokenizer pre-tokenizers for Qwen2 and Llama 3, run Qwen2.5-0.5B and 1.5B | Logits within tolerance of the oracle; greedy output equal for 64 tokens |
| M2 | `WeightProvider` trait; Resident and Lazy-map providers; load report and progress; planner v0 (RAM and measured disk speed) | Strategy-equivalence test passes; memory bound test passes |
| M3 | AVX2 kernels for Q4_K, Q6_K, Q8_0 dot products, threads | Equal to scalar; benchmark vs llama.cpp on the same machine reported |
| M4 | Server with streaming and cancel; moulid flag to choose the engine; A/B in QEMU | Boot test passes with the Rust engine; streaming visible in the desktop chat |
| M5 | Mixture-of-experts architecture support; MoE paging provider using `mouli-llm::weights::experts` | Equivalence with Resident on a small MoE; working-set measurement on a real MoE |
| M6 | Layer-streaming provider with read-ahead; batch scheduler; overnight batch mode | Equivalence test; throughput measured on a real file larger than RAM |
| M7 | Speculative decoding: n-gram first, then a draft model | Acceptance rate and speed measured on CPU |
| M8 | Domain slicing tool (offline, Rust) writing sliced GGUF with the robust-score report | A sliced model plus its report on a real model; quality loss stated |
| M9 | Mesh pipeline over `mouli-mesh` | Two machines produce the same output as one; per-hop latency measured |
| M10 | Cascade router and model memory manager (load, unload, escalate) | Escalation works without a restart; memory returns after unload |
| M11 | Remove llama.cpp from the image | Only after M1 to M4 pass on the target hardware and the owner accepts the speed gap |

Strategy 6 (activation-sparse reads) stays a research item until M0 and M6 results justify it.

## 11. Risks and limits

- Speed: llama.cpp has years of kernel tuning; expect a gap after M3 and plan to close it with measured work, or keep llama.cpp.
- Coverage: every new architecture needs code and tests; start with llama, qwen2, then one MoE family.
- Tokenizers: wrong pre-tokenization gives wrong text without errors; the oracle comparison catches it.
- Slicing: quality loss is real and task-dependent; the robust-score gate reduces, not removes, the risk.
- Mesh: latency and security; start with a trusted LAN.
- Effort (my estimate, not a measurement): M1 to M4 is weeks; M5 to M10 are weeks each; the whole is months of work.

## 12. Decisions needed from the owner

1. Keep llama.cpp as a fallback after M11, or remove it?
2. Accept the narrow `unsafe` modules in section 9?
3. First real models for parity and tests (suggested: Qwen2.5 0.5B and 1.5B, Llama 3.2 1B; one small MoE)?
4. Target hardware for benchmarks (the Zotac's CPU, RAM and GPU, plus the MacBook Air)?
5. Is a LAN model server (the "model box") in scope now or after M4?
