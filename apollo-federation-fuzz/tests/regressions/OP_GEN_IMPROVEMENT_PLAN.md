# Plan: improve operation generation beyond apollo-smith

The current op generator is `apollo-smith`'s `DocumentBuilder::operation_definition()`
plus a small post-pass in `op_gen.rs` that decorates inline fragments
and fragment spreads with `@skip`/`@include`/`@defer`. This document
captures the gaps observed during the cross-version sweep work
(Phases I–N) and a phased proposal for closing them — saved as a plan,
not yet executed.

## What apollo-smith gives us today

- A single anonymous or named `query` operation.
- Shallow nesting (typically 1–2 levels of inline fragments).
- Aliases on field selections — but only one alias per field, no
  collision pressure.
- `__typename` selections appear, but only at "natural" positions —
  not stress-tested on abstract / interface-object types.
- No named **fragment definitions** ever produced. Only inline
  fragments and (occasionally) anonymous fragment spreads — the
  latter referencing... nothing because the document has no
  `FragmentDefinition`s.
- No mutations. No subscriptions.
- No introspection queries.
- Variables only added by our post-pass (for `@skip`/`@include`).

## Why this matters

Several documented planner bug classes need shapes apollo-smith
doesn't reliably produce:

| Bug class                                        | Shape needed                                                                  | smith status |
|--------------------------------------------------|-------------------------------------------------------------------------------|--------------|
| FED-251 (`__typename` mishandling)               | `__typename` on shared / abstract root types, multiple times in same set      | weak         |
| FED-515 (type-conditioned fetching ambiguity)    | Inline fragments on abstract types with overlapping selections                | very weak    |
| Fragment normalization regressions               | Named `FragmentDefinition` + multiple `FragmentSpread` references             | **none**     |
| Operation-name collision in plan node naming     | Multiple operation definitions in one document (rare but legal)               | **none**     |
| `@defer` interaction with named fragments        | `... FragName @defer` (spread of a named fragment with a @defer at the spread)| **none**     |
| Path-conditioned merging                         | Same field repeated under sibling type conditions with different aliases      | weak         |
| Mutation planner paths                           | `mutation { ... }` operation with subgraph-ownership choices                  | **none**     |

The *negative results* in Phases J, L, N each happened against the
same shallow op shapes. We can't currently distinguish "no bug here"
from "no bug *findable with these op shapes*."

## Proposal — three phases

### Phase A: keep using smith, add a post-pass fragment-extractor
*Effort: small. Risk: low. Likely yield: closes FED-251 / fragment-norm gaps.*

Run smith as today, then post-process the AST:

1. Collect 1–3 leaf-or-near-leaf inline fragments from the operation,
   convert them into named `FragmentDefinition`s, and replace the
   originals with `FragmentSpread`s (same name, same type condition).
2. Optionally introduce **second** spread sites for the same fragment
   elsewhere in the document (where the type condition still applies).
3. For some fraction of operations, sprinkle extra `__typename`
   selections — particularly on entity-typed fields and
   interface-typed fields.
4. For some fraction, duplicate one selection with a fresh alias and
   layer `@skip(if: $v)` onto the alias copy — exercises path-
   conditioned merging.

This is roughly the same shape of post-pass we already have for
`@defer`. ~150–250 LOC total. No new dependency.

### Phase B: add a mutation surface
*Effort: small. Risk: low (the planner code path is similar to query). Likely yield: low against converged versions, useful for future.*

`apollo-smith` does support generating mutation operations. The
currently-hard-coded `operation_definition()` call can be widened to
randomly pick `mutation` (~15% of the time) when the supergraph has a
mutation root type — which our schema generator doesn't currently
produce. Two small pieces:

1. Generator: with low probability, emit a `Mutation` root type with
   1–3 `m<Entity>(input: ID!): <Entity>` fields, primary subgraph
   ownership (same rules as query).
2. op-gen: when the schema has a `Mutation` type, sometimes pick a
   mutation operation.

### Phase C: replace smith with a custom generator
*Effort: large. Risk: medium-high (regressions in op shape distribution may mask findings; need to A/B against smith for a while). Likely yield: high — full control over depth, fragment nesting, alias collisions, type-condition stacks.*

Only worth it if Phase A doesn't produce new findings within a few
sweeps. The custom generator should:

- Walk the api schema and produce shape-bounded selection trees with
  a configurable max depth and max breadth.
- Generate a small fragment library (1–3 named fragments per
  document) with intentional reuse from multiple spread sites.
- Support biased shape distributions: an `OpShapeDistribution` config
  that lets sweeps pick between "shallow flat" / "deeply nested" /
  "fragment-heavy" / "alias-collision-stress" profiles.
- Keep the existing `decorate_operation` post-pass for `@skip` /
  `@include` / `@defer`.

Design rough edges to think through before starting Phase C:
- Smith's deterministic fuzz/arbitrary integration is a real value
  add — any custom generator must consume `&mut Unstructured` so
  cargo-fuzz integration stays viable.
- Validity: smith guarantees the produced doc parses against the
  schema. A custom generator must do schema-aware selection picking
  *or* defer validation until after the generator runs and rely on
  the existing op_gen validation pass to filter invalid docs.
  Filtering wastes seeds but is simpler.
- Determinism contract: the seed → document mapping should be stable
  so regressions stay reproducible across generator versions
  (probably impossible to fully guarantee — accept that adopting the
  custom generator invalidates old seeds).

## Recommended order

Phase A → measure on a fresh sweep against (2.0.0, 2.13.1) and
(2.5.0, 2.13.1) → decide Phase C only if Phase A shows interesting
new categories or if specific bug classes (FED-251, FED-515) remain
unreachable.

Phase B is independent of A/C and can be done any time mutation
coverage becomes a real ask.

## What this plan does *not* try to do

- Semantic comparison (executor backend) — separate document, separate
  effort. Needs a deterministic seeded mock-subgraph.
- Schema-shape gaps that don't depend on op-gen (inter-entity refs,
  union types, multiple `@key` per entity with different key sets,
  newer fed directives) — handled in `COVERAGE_GAPS.md`.
