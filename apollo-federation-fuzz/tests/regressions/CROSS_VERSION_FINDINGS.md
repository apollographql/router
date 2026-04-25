# Cross-version exploration findings

This file records what the harness found when pointed at meaningfully older
`apollo-federation` baselines. Switch baseline by editing one line of
`apollo-federation-fuzz/Cargo.toml`:

```toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

The `harness_base.rs` adapter uses `..Default::default()` for `QueryPlanOptions`
and falls back to `Supergraph::new` (vs `new_with_router_specs`) so it spans
2.0.0 through HEAD without code changes.

## Summary

| Baseline | API drift | Raw divergence rate | After normalization |
|---|---|---|---|
| 2.13.0 (default) | none | 0/500 | 0/500 |
| 2.5.0 | none | 0/200 | 0/200 |
| 2.1.3 | `Supergraph::new_with_router_specs` and `QueryPlanOptions::disabled_subgraph_names` missing | 200/200 | 0/200 |
| 2.0.0 | additionally: `QueryPlanOptions::{check_for_cooperative_cancellation, non_local_selections_limit_enabled}` missing | 200/200 | 0/500 |

After accounting for **non-algorithmic** drift, the query planner produced
byte-identical plans across 13 minor versions and ~3 years of development for
this generator's directive surface (`@key`, `@shareable`, `@requires`).

## Categories of "divergence" the normalizer learned to ignore

These are wire/serialization differences, not planner algorithm differences:

1. `QueryPlanningStatistics.best_plan_cost` was added after 2.1.3; older
   versions don't emit it. The whole `statistics` subtree is now dropped
   from the normalized form (it's planner metadata, not the plan).
2. Older versions serialize absent options as `"foo": null`; newer versions
   added `#[serde(skip_serializing_if = "Option::is_none")]`. Null-valued keys
   are dropped during normalization.
3. The `requires:` selection set on entity fetches was a raw SDL string in
   2.0.0 (`"... on T0 { __typename id }"`) and a structured AST in newer
   versions. Normalizer renders both back to the same canonical SDL string.

If you find a divergence the normalizer has not yet learned about, add a
case here, then teach `diff::normalize` the rule.

## What this calibration does NOT cover

Zero divergences across 13 versions sounds great but really tells us the
**generator's directive surface is too narrow** to expose algorithmic
differences. To find real planner bugs, expand the generator (in priority
order):

1. Inter-entity field references (`type T1 { other: T2 }`) — most planner
   bugs live in entity resolution paths across subgraphs.
2. `@override` — transferring field ownership between subgraphs has subtle
   semantics and known historical bugs.
3. Multi-field keys (`@key(fields: "id type")`) and compound key references.
4. Interfaces with `@interfaceObject`.

Each of these expands the search space the planner has to navigate; older
versions had less mature optimization for several of them.
