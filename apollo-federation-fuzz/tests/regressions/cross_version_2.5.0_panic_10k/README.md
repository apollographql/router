# 10k-op pressure-test sweep — found a planner panic in HEAD

After Phases I → Q saturated the directive surface against 2.5.0 → 2.13.1
at the 1k-op scale (CROSS_VERSION_FINDINGS.md), one 10× pressure-test
sweep at the most stressing config surfaced a real planner bug.

## Setup

- BASE = `=2.5.0` (in-window, previously thought converged with HEAD)
- HEAD = in-tree 2.13.1
- 10000 ops over 2000 supergraphs
- `--enable-defer` (so `@defer` decoration is exercised)
- Full Phase A op-gen post-passes active (`__typename`, alias-with-skip,
  fragment extraction)
- Full schema directive surface active (compound `@key`, multi-`@key`,
  multi-field `@requires`, `@override` w/ progressive labels, `@external`,
  `@shareable`, `@provides`, interfaces, `@interfaceObject`, inter-entity
  refs)

The 10k sweep would have aborted at iteration 2028 when the in-tree
planner panicked. That was the trigger to add a `catch_unwind` wrapper
to `run_diff` (new `DiffOutcome::PanickedSide` variant) so a single
planner panic doesn't take out the rest of the sweep.

## Result

    Stats { schemas_attempted: 2000, schemas_composed: 2000,
            schemas_compose_failed: 0, ops_attempted: 10000,
            ops_skipped: 0, planned_identical: 9999,
            planned_divergent: 0, planner_errored: 0,
            panicked: 1 }

- **0 plan divergences** across 9999 ops on the 2.5.0 → 2.13.1 pair.
  Saturation against this version pair confirmed at 10× scale.
- **1 planner panic** at iteration 2028. Reproduces deterministically
  on both BASE (2.5.0) AND HEAD (2.13.1) with the same assertion
  message — meaning **this is a still-present bug in the in-tree
  apollo-federation 2.13.1**, not a previously-fixed one.

## The panic

```
thread 'main' panicked at apollo-federation/src/query_plan/fetch_dependency_graph.rs:1899:9:
Root nodes should have no remaining nodes unhandled, but got: [11 (missing: [7])]
```

The assertion lives in
[`FetchDependencyGraph::process_root_nodes`](../../../apollo-federation/src/query_plan/fetch_dependency_graph.rs#L1880)
and expresses a planner invariant: after processing root nodes, no
unhandled fetch nodes should remain. When this fires, the planner has
left an unreachable fetch node in the dependency graph — a real
correctness bug rather than a graceful "I can't plan this" rejection.

The backtrace shows the panic happens deep inside the recursive
`process_root_nodes` walk, called from `QueryPlanningTraversal::cost`
(plan-evaluation phase), via `generate::generate_all_plans_and_find_best`.
So the planner crashes during *plan-cost evaluation* on a specific
combination of plan branches — implying the bug lives in how the
fetch graph for nested-defer + entity-traversal plans gets constructed.

## What triggers it

The reproducer (`process_root_nodes_panic.txt`) has these features
co-occurring:

1. **4 subgraphs** (s0–s3) hosting overlapping entity definitions.
2. **Inter-entity reference**: `T2.r2_0: T0` declared in s0, s2, s3.
3. **Multiple fed directives**: `@override`, `@requires`, `@external`,
   `@shareable`, `@provides`, single + multi-`@key`.
4. **Deeply nested `@defer`**: at least 3 levels of `... @defer { ... @defer { ... } }`,
   one of them on the inter-entity reference traversal `r2_0`.
5. **`@skip` and `@include`** interleaved with the `@defer` blocks.
6. **A named fragment spread `...Frag1 @defer`** (Phase A fragment
   extraction).
7. **Alias-with-skip duplicates** (Phase A) of the same field at
   multiple positions.

Reducing any one of those lifts the panic into something else — i.e.
this is a multi-feature interaction bug, not a single-directive
bug. Multi-hop entity traversal under nested `@defer` looks like the
core of it; defer plan-cost evaluation has to walk through a fetch
graph that has both deferred and non-deferred paths to the same node.

## Why 1k-op sweeps missed this

At 1k ops the trigger probability is ~1/2000 (we hit it at iter 2028);
roughly 50% chance per 1k. The four 1k sweeps that ran with
`--enable-defer + Phase A + full directives` (Phase P, Phase Q,
2.5.0 in-window confirmations) collectively saw ~5k ops — an
"unlucky" run could have hit it earlier. The reason it surfaced in
this particular 10k run is the cumulative coverage of every Phase I → Q
generator addition simultaneously, plus the binary not aborting on
the panic so we know what was running when it crashed.

## Class E rate confirmation

The same 10k sweep against BASE = `=2.0.0` (10000 ops, --enable-defer)
yielded:

| Pattern | 1k sweep | 10k sweep |
|---|---:|---:|
| Total divergences | 552 | 5702 |
| PR #7580 (class A) | 188 | 1935 |
| Condition (class B) | 401 | 4180 |
| sub_selection (class C) | 523 | 5339 |
| Field repr (class D) | 455 | 4616 |
| **Outside A/B/C/D** | 0 | **3** |
| Panics | 0 | 1 |

3 instances of "outside A/B/C/D" at 10k → confirms Class E (alias-
duplicate reordering) fires at ~0.05% rate consistently. The single
1k-sweep instance was not a fluke.

## Curated artifact

- `process_root_nodes_panic.txt` — the full reproducer:
  4 subgraph SDLs, composed supergraph, the triggering operation,
  and the panic message. Replays deterministically with seed 17
  iter 2028 in `bin/fuzz` once `--iterations 2029` is reached.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml — either base; the panic is in HEAD
apollo-federation-base = { package = "apollo-federation", version = "=2.5.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 10000 --seed 17 --ops-per-schema 5 --enable-defer \
    --regressions-dir tests/regressions/cross_version_2.5.0_panic_10k
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
