# Where the code came from

Provenance of everything pulled into this repository from the local `sources/` collection, what was left out and why, and what is still unverified. Written 2026-11-23. Keep it current when sources are mined further.

"Ported" means *rewritten in Rust with its own tests*, not copied. "Verified" always names the check that was run.

## Taken

| Source | Went into | Notes |
|---|---|---|
| `AEON-main-full` (C core, `aeon-os`) | `aeon-tensor`, `aeon-model`, `aeon-engine`, `aeon-identity`, `aeon-cognition`, `aeon-runtime`, `aeon-adapters`, `aeond`, `aeon-hw` | Module-by-module map and the non-Rust remainder in [`PORT_STATUS.md`](PORT_STATUS.md). |
| Theory toolkit (`TheoryOfIntelligence`, `stem_one_click_v11`) | `aeon-theory` | Robust-S, gap law, calibration, cascade, selection, TIES merge. |
| 55D research engine (`stem_one_click_v11`) | `aeon-compress` | Model-independent numerics only: entropy, outliers, near-duplicates, quantisation error, pruning threshold, singular values, effective rank, low-rank error, RVQ sizes. |
| `AgentBox-main` | `aeon-forge` (manifest, registry, candidate mutation, evaluation, capability contracts, fitness, comparison), `aeon-memory::context` | Pure logic only; see "Left out". |
| `topstar-main` | `aeon-topstar` | The upstream snapshot did not compile or pass its tests; repairs listed in the crate README. |
| `AEONOS` (user-provided) | `aeon-agents`, `aeon-architecture`, `aeon-business`, `aeon-mail`, `aeon-project`, `aeon-studio`, `aeon-system`, `aeon-web`, ... | Docs and small binaries; `mail`, `web`, `agents` merged into existing crates. |
| Archive chats (`archive-code`) | `aeon-core`, `aeon-agents`, `aeon-sched`, `aeon-neuro`, `aeon-mesh`, `aeon-memory`, `aeon-web`, `aeon-comms`, `aeon-store` (earlier), plus mesh repair/aggregation, neuro eligibility traces, working memory, ontology | Concepts re-expressed in Rust; the archive code itself is unreviewed ChatGPT output. |
| Project specs (`TheoryOfIntelligence/prompts`) | `aeon-gguf`, `aeon-quant`, `aeon-tokenizer`, `aeon-llm` | Written from the GGUF format and the engine plan (milestones E1, E2), not from other engines. |

`aeon-memory::chunk` is original code (the RAG archive chat contained only file trees).

## Left out, deliberately

* **AgentBox**: the FastAPI service, SQLite persistence, OIDC/JWT auth, AES-GCM tenant backup, PostgreSQL connectors, the OpenAI provider, execution job queue and worker, evidence verification against stored executions, the gVisor/Docker sandbox runner (AEON isolates services with Landlock/seccomp, see `AEON_RUST_PLAN.md`), and the `json_schema_valid` assertion (needs a JSON-Schema crate).
* **LibreOne**: its modules are 12 to 19 line stubs; nothing to port.
* **YNDRA**: `language/` (90 files, a separate compiler/runtime project with its own handoff history) and `architecture/` are a sibling project, not AEON. `recovered/` (91k files) is raw chat fragments, mostly text, shell lines and Flutter UI; the AEON-relevant parts were already mined through the archive.
* **Archive topics that are not AEON**: finance and crypto tools, SAP/ERP/marketing apps, resume and income-plan chats, Bitcoin/mnemonic scripts, the AetherLang compiler (sibling), supply-chain engines.
* **Kernel-space and boot code** (device driver, UEFI network, eBPF/netfilter attachment): stays non-Rust by the Rust plan.
* **The generated "execution strategy" scaffold** (about 700 thin Python wrappers): only working memory and the ontology had behaviour worth carrying over.

## Not yet verified

These were checked against tests written alongside them, **not** against a reference implementation on real data:

* `aeon-gguf`: parser tested by round trips, malformed inputs, truncation at every byte and 10,000 random mutations; not yet run on real model files.
* `aeon-quant`: block layouts from the published format; tests use hand-built blocks. The E1 comparison with a reference engine on real tensors is open.
* `aeon-tokenizer`: only the GPT-2 pre-tokenizer exists, so models with another `tokenizer.ggml.pre` may split differently; no comparison with a reference tokenizer yet.
* `aeon-llm`: compared with an independent naive implementation on tiny synthetic models; no real-model logits comparison (E2); scalar and single-threaded.
* C-to-Rust differential tests (identity, semantic wire, tensor): not run; C files have not been removed.
* `aeon-topstar`, `aeon-compress`, `aeon-forge`, `aeon-memory`: unit-tested only.

## How to mine further

1. Find the concept in `SOURCES_INVENTORY.md` (outside the repo) or the archive `INDEX.tsv` files.
2. Check whether a crate here already covers it (`PORT_STATUS.md`, this file).
3. Port the behaviour, not the file layout; add tests that would fail if the behaviour were wrong; run `cargo fmt`, `cargo clippy -D warnings`, `cargo test`.
4. Add a line here.
