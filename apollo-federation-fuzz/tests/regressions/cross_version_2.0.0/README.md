# Cross-version 2.0.0 sweep — what the harness found

After Phase F (operation `@skip`/`@include` decorator + renamed root types)
the harness was pointed at `apollo-federation = "=2.0.0"` as the baseline
and run for 1000 ops across 200 generated supergraphs:

    Stats { schemas_attempted: 200, schemas_composed: 200,
            schemas_compose_failed: 0, ops_attempted: 1000,
            ops_skipped: 0, planned_identical: 905,
            planned_divergent: 95, planner_errored: 0 }

**95 real divergences (9.5% of ops).** Every single one is the same
underlying difference in the planner's emitted subgraph operation:

```
- ... on Query @include(if: $__v1) { ... }   # 2.0.0 (baseline)
+              ... @include(if: $__v1) { ... }   # 2.13.1 (HEAD)
```

This is **literally PR #7580** from the router CHANGELOG:

> "The query planner was adding an inline spread (`...`) conditioned on the
> `Query` type in deferred subgraph fetch queries. Such a query would be
> invalid in the subgraph when the subgraph schema renamed the root `query`
> type to something other than `Query`. The fix removes the root type
> condition from all subgraph queries, so that they stay valid even when
> root types are renamed."

The harness rediscovered the documented bug fix end-to-end:

1. The Phase-C subgraph generator produced a federated graph (in this
   showcase: two subgraphs sharing entities `T0`, `T1` via `@key(id)`).
2. Apollo-smith generated a base operation against the api schema.
3. The Phase-F decorator added `Boolean!` variables and sprinkled
   `@skip`/`@include` directives, including on inline fragments.
4. Both `apollo-federation` 2.0.0 and 2.13.1 were asked to plan the same
   operation against the same supergraph SDL.
5. They produced different subgraph fetch operations — the buggy 2.0.0
   form vs the fixed 2.13.1 form.
6. The diff layer flagged it; the binary saved
   `PR_7580_rediscovery.txt` with the full reproducer.

`PR_7580_rediscovery.txt` contains the smallest captured case (1
subgraph fetch). The other 94 (deleted) all exhibited the same pattern
on more complex plans.

## Reproducing this run

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 42 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0
```

The default `Cargo.toml` is back to `=2.13.0` so `cargo test` is clean.
