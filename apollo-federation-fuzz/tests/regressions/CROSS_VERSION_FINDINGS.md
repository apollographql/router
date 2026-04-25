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

## State after Phases I → Q (>= 41000 ops swept)

The harness now exercises the full schema/operation surface from
`COVERAGE_GAPS.md`:

- Schema directives: single + compound `@key`, multiple `@key` per
  entity (different key sets), single + multi-field `@requires`,
  `@override` (with and without progressive labels), `@external`,
  `@shareable`, `@provides`, interfaces, `@interfaceObject`,
  inter-entity reference fields with multi-hop traversal.
- Operation shaping (op_gen post-passes): `@skip` / `@include` /
  `@defer` decoration, `__typename` sprinkling, alias-with-skip
  duplication, named `FragmentDefinition` extraction.

## Summary by baseline

The default sweep is 1000 ops over 200 generated supergraphs at seed 17.
Plans compared as JSON via `diff::normalize`.

| BASE → HEAD (in-tree 2.13.1) | no-defer | --enable-defer (multi-seed) | Divergence classes hit |
|---|---|---|---|
| `=2.0.0`  | 84  | 552 | A, B, C, D, **E** |
| `=2.1.3`  | 87  | 201 | A, B, C, D |
| `=2.5.0`  | 0   | 0 (4 seeds × 1000 = 4000 ops) | none |
| `=2.10.0` | 0   | 0 (1000 ops) | none |
| `=2.13.0` | 0   | 0 (1000 ops) | none |

> **Fix window for every divergence class: (2.1.3, 2.5.0].** Confirmed
> by three converged in-window versions (2.5.0, 2.10.0, 2.13.0).

## Divergence classes characterised

### A. PR #7580 — extraneous `... on Query` inline fragment

The 2.0.0/2.1.3 planner emits subgraph operation documents containing
`... on Query @skip(if: $v) { ... }` even when the root field is not
renamed. HEAD emits `... @skip(if: $v) { ... }`. Both are valid
GraphQL, but the inline-on-Query form is a known bug fixed in
[PR #7580](https://github.com/apollographql/router/pull/7580) and was
the first finding on this version pair, surfaced before any of the
schema-shape generator additions.

### B. FED-505 — missing `Condition` plan node

When an op has `... @skip(if: $v) { ... }` at a position that could
elide an entire subgraph fetch, HEAD wraps the affected `Fetch` in a
`Condition { condition_variable, else_clause }` plan node (so the
fetch is skipped at runtime when the variable is true). 2.0.0/2.1.3
keep the fetch flat and let the subgraph receive a request that filters
out everything — over-fetching. Documented as
[FED-505](../../apollo-federation/src/correctness/query_plan_analysis_test.rs#L321):

> "QP missing ConditionNode bug (FED-505).
>  - Note: The correctness checker won't report this, since it's an
>  over-fetching issue."

The harness rediscovered the underlying class without prior knowledge
of FED-505 — it just exercised the surface (interfaces +
`@skip`/`@include`) that triggers it.

### C. Defer wire-format: `Defer.primary.sub_selection` field added

When `--enable-defer` is on, HEAD's `Defer.primary` plan node carries
a `sub_selection` string summarising the immediate (non-deferred)
selection. 2.0.0 doesn't emit the field at all.

```diff
 "Defer": {
   "primary": {
+    "sub_selection": "{ qT0 @include(if: $__v1) { ... } qT0 { ... } }",
     "node": { "Fetch": { ... } }
   },
```

Likely a deliberate executor-coordination addition rather than a bug
fix; documenting it as wire-format drift.

### D. Defer Field representation: string-with-directives → `{response_key}`

In `Defer.deferred[*].query_path`, `Field` entries are encoded
differently:

```diff
 "Field": "qI0 @skip(if: $__v0)"
+"Field": { "response_key": "qI0" }
```

2.0.0 stringifies the path element with the runtime-conditional
directive baked in. HEAD uses a structured form with just the response
key. Looks like a real semantic fix — runtime conditions shouldn't
couple plan structure to variable values.

### E. Selection reordering for aliased duplicates *(new in Phase P)*

When a selection set contains aliased duplicates of the same field,
HEAD reorders them in the emitted subgraph operation document; BASE
preserves declaration order:

```diff
-"qT1 { ... id @skip(if: $__v0) aS4: id @skip(if: $__v0) aS3: id @skip(if: $__v1) }"
+"qT1 { ... aS3: id @skip(if: $__v1) id @skip(if: $__v0) aS4: id @skip(if: $__v0) }"
```

Surfaced once Phase A (op_gen post-passes) added alias-with-skip
duplication. Two independent instances captured (Phases P and Q).
Likely a canonicalisation/sort added in HEAD; both orderings are
valid GraphQL, so almost certainly benign. Could not have been seen
before alias duplicates existed in generated ops.

## Phase-by-phase findings index

For full per-phase methodology, sweep counts, and curated artifacts,
see the per-directory READMEs:

| Phase | Surface added                                             | New class? |
|-------|-----------------------------------------------------------|------------|
| I     | multi-field `@requires` (`fields: "f g"`)                | none       |
| J     | compound `@key` (`fields: "id k0"`)                       | none       |
| K     | bisection sweep (2.1.3, 2.13.0 baselines)                 | none       |
| L     | `@provides` on Query root                                 | none       |
| M     | first defer-enabled sweep (`--enable-defer`)              | **C, D**   |
| N     | `@interfaceObject`                                        | none       |
| O     | inter-entity reference fields (multi-hop traversal)       | none       |
| P     | op_gen Phase A: `__typename`, alias-skip, fragment defs  | **E**      |
| Q     | multiple `@key` per entity (different key sets)           | none       |

## Categories of non-algorithmic drift the normalizer ignores

These are wire/serialization differences, not planner algorithm
differences. They are normalised away by `diff::normalize` and don't
appear in the divergence classes above:

1. `QueryPlanningStatistics.best_plan_cost` was added after 2.1.3;
   older versions don't emit it. The whole `statistics` subtree is
   dropped from the normalised form (planner metadata, not the plan).
2. Older versions serialise absent options as `"foo": null`; newer
   versions added `#[serde(skip_serializing_if = "Option::is_none")]`.
   Null-valued keys are dropped.
3. The `requires:` selection set on entity fetches was a raw SDL
   string in 2.0.0 (`"... on T0 { __typename id }"`) and a structured
   AST in newer versions. Normaliser renders both back to canonical
   SDL.

If you find a divergence the normaliser has not yet learned about,
add a case here, then teach `diff::normalize` the rule.

## What the harness still does NOT cover

After Phases I → Q the directive/schema-shape coverage is broad
enough that adding more directives yields negative results (4000+
ops at 2.5.0 with each new directive: 0 divergences). The remaining
gaps are not directive shape but qualitatively different:

1. **Operation shape distribution beyond apollo-smith.** The
   op_gen Phase A passes widen what smith produces but smith is
   still the structural backbone. Phase C (custom generator) is
   captured in [`OP_GEN_IMPROVEMENT_PLAN.md`](OP_GEN_IMPROVEMENT_PLAN.md).
2. **Newer federation features.** `@context` / `@fromContext`
   (fed v2.8+), `@requiresScopes`, `@policy`. Each has its own
   planner code path and would require a fed-2.8+ baseline to test,
   which 2.5.0 cannot parse.
3. **Mutations and subscriptions.** Listed in `COVERAGE_GAPS.md`
   row 9 as low priority. Phase B in the op-gen plan covers it.
4. **Semantic comparison via mock executor.** Currently we only
   diff plans. Catching "both planners produce the same wrong
   plan" needs an executor backend that responds deterministically
   from a seed. Not yet attempted; would need its own design.

The empirical claim with current coverage is bounded but precise:

> Across compound `@key`, multiple `@key` per entity, multi-field
> `@requires`, `@override` (incl. progressive), `@external`,
> `@shareable`, interfaces, `@interfaceObject`, `@provides`,
> inter-entity references with multi-hop traversal, `@defer`,
> `__typename` sprinkling, alias-with-skip duplication, and named
> fragment definitions — the planner from **2.5.0** forward is
> byte-identical to **2.13.1** on every generated case across
> 11000+ ops at three converged in-window versions (2.5.0, 2.10.0,
> 2.13.0).
