# MOULI-CODE core: implementation status

This file records what the Rust crate in this folder actually implements. `README.md` and
`MOULI-CODE.md` are the product specification and say what MOULI-CODE should become; do not read
them as a statement of what is built.

## Implemented (`mouli-code` crate, no dependencies, no file-system or process access)

| Module | What it does |
|---|---|
| `diff` | Unified-diff types; strict parser (line counts must match the headers, creation/deletion, `\ No newline at end of file`, git headers and trailing prose tolerated, renames refused); renderer; reverse. |
| `apply` | Exact application with every context and removed line verified; optional bounded offset search; end-of-file newline handling; atomic multi-file patch sets applied to a copy; path safety (no absolute paths, `..`, `.git`, backslashes, control characters). |
| `make` | Diff generation (prefix/suffix trim plus bounded LCS), merged hunks, removals before additions as git does. |
| `quality` | Patch statistics; test-file detection by path component; hard-coding and special-casing heuristics on ADDED lines only; syntax check of the EDITED files; risk score; blocking versus warning findings. |
| `minimize` | Delta debugging over hunks with an oracle and a call budget; creations and deletions are never split. |
| `budget` | Hard limits on iterations, model calls and repair attempts. |

## How it was checked

* Real `git` is the oracle in both directions: `git apply` accepts every diff this crate
  generates and produces the same file, and diffs made by `git diff` parse and apply identically.
* 1,500 randomized edits (including missing final newlines and empty lines) round-trip through
  generate, apply, reverse and render/parse.
* The quality policy is tested against the patched files, not the originals.

## Origin and what was NOT carried over

The ideas come from a Python prototype ("gemmaAgent"). Reading it showed that its core pieces were
placeholders: `basic_diff_apply` returned its input unchanged, the real apply shelled out to
`git apply --unsafe-paths` (which switches off git's path check), `rollback` reverted every
tracked change instead of the agent's, `PatchMinimizer.try_minimize` returned the patch
untouched, and the quality and hard-coding checks ran on text that was never patched. This crate
implements what those pieces were meant to do.

## Not implemented yet

* Repository code graph and retrieval (symbol graph, BM25 text search, hop expansion within a
  context budget).
* Test execution and result parsing (needs a sandboxed process runner).
* Language-aware syntax checks (only a delimiter-balance guard exists).
* Model providers, prompting and the agent loop itself.
* Writing to the real file system, with backup and exact rollback of the agent's own change
  (the pure `reverse` patch makes this straightforward; the I/O layer is not written).
* The hard-coding and special-casing checks are heuristics that flag patterns for review; they
  cannot prove a patch is honest.
