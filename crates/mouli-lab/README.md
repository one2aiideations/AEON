# mouli-lab

Research harness for the Aetheron dendritic model, standard library only. It is kept apart from the
product crates.

- `aetheron_ml`: dendritic layer (branches, batch norm with an EMA path, straight-through gate, attention across units) with hand-written forward and backward passes.
- `baselines`: MLP with Adam, and online EWC.
- `continual`: a Split-MNIST style benchmark (none, replay, schedule, EWC, joint) that labels results honestly (a strategy that only exists for the MLP is reported as `mlp`).
- `temporal`: recurrent, gated and one-layer transformer models on a delayed-match task, with full back-propagation through time.
- `data`, `linalg`, `rng`, `report`, `core`: IDX loading, small matrices, a seeded generator, run files and aggregation.

## What is verified

`cargo test -p mouli-lab` (8 tests) checks every hand-written gradient against finite differences for
every parameter array of every model, parses damaged IDX input safely, and checks that a run is
reproducible. A deliberately broken batch-norm gradient makes the test fail.

## What is not

Nothing has been trained on the real MNIST files, so no accuracy or forgetting numbers from the
original paste have been reproduced. Do not quote any result from this crate until it has been run on
real data.
