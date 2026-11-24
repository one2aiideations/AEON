# mouli-agentmem

Agent-memory building blocks, standard library only. Adapted from the AgentOS m022 to m089 sketches;
each module is self-contained.

Groups: provenance, consolidation, decay, access control, audit journal; snapshot, wire and
checkpoint codecs; replication log, merge, anti-entropy, repair, scheduler, dispatcher and replay
guard; the replay-delta chain (x050 to x089); and HNSW search, a timeseries store, a schema migrator,
a WAL change reader and a capability policy.

`cargo test -p mouli-agentmem` runs 173 tests. Adapting the code found real defects (missing `Ord`,
a use-after-move, an unwrapped `Result`) and several tests that asserted the wrong error; both were
fixed. Not tested under load or on hardware. The modules that need `sha2`, `serde` or async crates
were left out pending a dependency review.

`missing_docs` is allowed here for now (about 500 undocumented fields and variants).
