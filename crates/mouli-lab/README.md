# mouli-lab

Research harness for the Aetheron dendritic model, standard library only. It is kept apart from the
product crates.

- `aetheron_ml`: dendritic layer (branches, batch norm with an EMA path, straight-through gate, attention across units) with hand-written forward and backward passes.
- `baselines`: MLP with Adam, and online EWC.
- `continual`: a Split-MNIST style benchmark (none, replay, schedule, EWC, joint) that labels results honestly (a strategy that only exists for the MLP is reported as `mlp`).
- `temporal`: recurrent, gated and one-layer transformer models on a delayed-match task, with full back-propagation through time.
- `data`, `linalg`, `rng`, `report`, `core`: IDX loading, small matrices, a seeded generator, run files and aggregation.

## Command line

```
mouli-lab selftest                       # every gradient against finite differences
mouli-lab run --out DIR --mnist TRAIN_IMG TRAIN_LBL TEST_IMG TEST_LBL
              [--arch aetheron,mlp,aetheron-no-attention,aetheron-no-gate,aetheron-no-homeostasis]
              [--strategy none,er,sched,ewc,joint] [--seeds N] [--epochs N]
mouli-lab run --out DIR --synthetic ...  # machinery check only; labelled NOT MNIST everywhere
mouli-lab temporal --out DIR [--model recurrent,gated,transformer] [--seeds N] [--steps N]
mouli-lab aggregate DIR                  # report with wins and losses
```

`run` refuses to start without the four MNIST IDX files unless `--synthetic` is given. Learning
rates are tuned once per method on a held-out tenth of the training data, with the same grid and
fixed seed for every method, then each seed is run. The report compares Aetheron with the MLP, and
each ablation with the full model, seed by seed, for accuracy and for forgetting, with a bootstrap
interval and an exact sign-flip p-value, and says when there are too few seeds to conclude.

## What is verified

`cargo test -p mouli-lab` checks every hand-written gradient against finite differences for
every parameter array of every model, parses damaged IDX input safely, and checks that a run is
reproducible. A deliberately broken batch-norm gradient makes the test fail.

## What was and was not reproduced from the original paste

Reproduced (as code, checked by tests): the dendritic layer with branches, batch norm with an EMA
path, the straight-through gate and attention across units, with hand-written backward passes that
match finite differences; the MLP and online EWC baselines; the Split-MNIST style protocol with
its five strategies; the delayed-match task with the recurrent, gated and transformer models. The
benchmark code, the seed sweep, the tuning budget and the report are written here and run.

Not reproduced: no accuracy, forgetting or ranking from the original paste. Nothing has been
trained on the real MNIST files, because they were not supplied; every number this crate has ever
printed came from synthetic data and says nothing about MNIST. The Aetheron-versus-MLP comparison
has therefore never been run on real data, and on the synthetic data used so far with four seeds
the report itself states that four seeds are too few to conclude anything. The ablations exist
and run, but no ablation result on real data exists. Do not quote any result from this crate until
`mouli-lab run --mnist ...` has been run with enough seeds (the report shows the interval).

Two details that differ from "a plain port": `Joint` and `EWC` exist only for the MLP, so asking
for Aetheron with them gives a result labelled `mlp`; and "attention off" removes the attention
(identity matrix, no gradient into query/key), it does not merely freeze it.

Speed: `matmul_t` is 3.3 times faster than the plain loop with bit-identical results (a rerun of a
whole benchmark gave identical result files). The other matrix routines and the attention loops
have not been profiled or changed.

## What is not

Nothing has been trained on the real MNIST files, so no accuracy or forgetting numbers from the
original paste have been reproduced. Do not quote any result from this crate until it has been run on
real data.
