# Theory of Intelligence inside MOULI OS (native)

Status: plan only. Nothing here is implemented or verified.

## Decision

The theory toolkit's **concepts and algorithms become native MOULI code** (C, on the MOULI engine). The
OS image does not ship the Python toolkit, the paper PDF or the ShiftBench files. The Python code in
`integrations/theory/` stays in the repository as the **reference implementation and test oracle**
only. Each ported piece must reproduce the reference results on fixed test inputs before it is
called done.

## Map: concept → native MOULI module

| Concept (reference) | What it does in the OS | Native module | PR |
|---|---|---|---|
| Closure test: shift conditions and episode construction (`theory_layer`) | Builds prompts under controlled distribution shifts to test whether a model relies on shortcuts | `userspace/models/closure.c` | PR 3 |
| Scorer (`HFScorer`) | Gets option scores from a model | engine API: score-continuation call | E2 |
| Robust score S, `summarise`, `q_of` | Worst-case normalised performance over the shift closure; shown for every model | `userspace/models/robust_s.c` | PR 3 |
| Benchmark-gap law | Predicts accuracy drop when shortcuts stop working | `userspace/models/gap_law.c` | PR 3 |
| Calibration (`calibration`) | Reliability of model confidence (binned) | `userspace/models/calibration.c` | PR 3 |
| Cascade planner (`plan_cascade`) | Chooses small→large model routing for a quality target and cost | `userspace/models/cascade.c` | PR 3 |
| Leaderboard / `choose_by_robust_score` | Ranks local models; picks the one to use | `/models`, `/use` | PR 3 |
| Discovery and header-only checks (`model_forge`) | Hugging Face search, safetensors/GGUF header fit | `userspace/models/search.c` | PR 3 |
| Task-vector screening, TIES soup (`merge`, `screen`, thresholds) | Streamed merge of compatible models; kept only if held-out S does not drop | `userspace/models/merge.c` | PR 3 |
| Sparse runtime: tiered experts (`sparse_runtime`) | Run large MoE models under a RAM budget | `core/mouli_expert_store.c` (already in C) | done |
| Hot-set profiling, S-checked pruning, dense neuron slicing | Reduce what stays in fast memory without losing S | `userspace/models/prune.c` + engine hooks | after E4 |
| Hardware plan (`hw`) | Threads, context, precision, budget | `userspace/moulid/mouli_hw.c` | done (extend) |
| 55D profile (`fiftyfive_*`, 55D engine v3.1) | Full profile of a model: language, coding, knowledge, long context, pruning sensitivity, energy | `userspace/models/profile55d/` | staged, below |

## 55D, in stages

The reference 55D engine is about 6,000 lines of Python on PyTorch plus about 700 lines of benchmark
extras. It needs per-layer access to the model, so the MOULI engine gets a **probe API in E2**:
layer outputs, attention maps and logits taps.

Experiments from the reference benchmark, each ported only when its test matches the reference:

| Experiment | Needs | Status |
|---|---|---|
| Language: windowed negative log-likelihood | engine logits | planned |
| Multiple-choice accuracy | engine scoring | planned |
| Coding: generate and run small tasks | engine plus a sandboxed runner (PR 4) | planned |
| Knowledge retention under pruning | probe API plus pruning | planned |
| Long-context retrieval at several depths | engine, large KV cache | planned |
| Energy per token (RAPL) | Linux power interface | planned |
| Recovery after pruning (short fine-tune) | **training**, which the engine does not do | out of scope for now |

The 124 classes and functions in the 55D engine file have not yet been inventoried. Before PR 3 ends,
list them, mark each as ported, planned or dropped, and state why.

## Open points (stated up front)

- Closure test items come from public datasets today. The OS must work offline, so item sets must be
  produced and stored locally at build or first run. The source and licence of those items must be
  settled; this is a decision for the owner.
- Matching the Python reference on a fixed fixture is the correctness bar. Matching it on real models
  is unverified until run.
- Training-based pieces (recovery fine-tuning) are not planned for the engine.
