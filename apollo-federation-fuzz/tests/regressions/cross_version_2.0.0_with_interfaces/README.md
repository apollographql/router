# Interface surface sweep against 2.0.0 — new finding

After Phase H (added interface declarations + `qI0: I0` root field +
`implements I0` on selected entities), pointed the harness at
`apollo-federation = "=2.0.0"` for 1000 ops over 200 generated supergraphs:

    Stats { schemas_attempted: 200, schemas_composed: 200,
            schemas_compose_failed: 0, ops_attempted: 1000,
            ops_skipped: 0, planned_identical: 908,
            planned_divergent: 92, planner_errored: 0 }

92 divergences (9.2%). Categorization by inspecting the diff section of each
saved repro:

| Category | Count | Description |
|---|---|---|
| PR #7580 only (`... on Query` extraneous root condition) | 32 | Same pattern as previous sweep |
| `Condition` plan node added in HEAD | 0 | (none pure — always co-occurs with PR #7580 in our regressions) |
| Both PR #7580 *and* `Condition` node | 60 | New finding |

**60 divergences contain a real `Condition` plan node difference.** This is
a *new* algorithmic divergence the harness had not surfaced before — it
required interfaces (and the `... on T0` inline fragments apollo-smith
generates against an interface-typed root field) combined with the
`@skip`/`@include` decorator.

## What the difference looks like

Same operation, same supergraph, two compiled planner versions (linked
into the same binary):

```
2.0.0 (base):                    2.13.1 (head):

"Fetch": {                       "Condition": {
  "operation_document":            "condition_variable": "__v0",
    "query(...) {                  "else_clause": {
      ... @skip(if: $__v0) {         "Fetch": {
        ... actual selection ...      "operation_document":
      }                                 "query(...) { ... actual selection ... }"
    }",                              }
}                                  }
                                 }
```

The newer planner extracts a top-level `@skip(if: $v)` into a `Condition`
plan node so that when the variable is `true` the entire subgraph fetch is
elided — no network round-trip, just zero data. The 2.0.0 planner kept the
fetch in place and let the subgraph filter, fetching unnecessary data.

This is the **FED-505 / over-fetching** family of bugs documented in
[`apollo-federation/src/correctness/query_plan_analysis_test.rs:321`](../../../apollo-federation/src/correctness/query_plan_analysis_test.rs#L321):

> "QP missing ConditionNode bug (FED-505).
>  - Note: The correctness checker won't report this, since it's an
>  over-fetching issue."

The harness rediscovered the underlying class without any prior knowledge
of FED-505 — it just exercised the surface (interfaces + `@skip`/`@include`)
that triggers it.

## Curated artifacts

- `FED505_condition_node_added.txt` — the cleanest pure-Condition diff.
  Operation has `... @skip(if: $__v0) { qT0 ... }` at the root; HEAD wraps
  the s1 fetch in a `Condition` node, BASE keeps it as a flat `Fetch`.
- `interface_with_condition.txt` — interface present in the schema, op
  uses `... on T_i { ... }` inline fragments inside `qI0`, both PR #7580
  and Condition divergences appear in the same plan.

## Reproducing this run

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_with_interfaces
```

The default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
