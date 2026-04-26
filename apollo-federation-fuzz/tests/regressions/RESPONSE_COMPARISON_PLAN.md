# Plan: response-equivalence comparison via deterministic mock executor

Captures an idea raised earlier in the harness work and explicitly
deferred. Saved here so it doesn't get lost; not in scope for the
planner-optimization performance work that motivates the rest of
the harness.

## What it is

The current diff layer compares **plan JSON** between two planner
versions. That catches "the planners produced *different* plans" but
**doesn't catch "the planners produced the same plan and the plan is
wrong"**. The latter happens whenever a bug is in shared planner code
or in shared composition logic — both versions agree on the wrong
answer.

Response-equivalence comparison closes that gap by comparing **what
the executor would return** rather than the plan structure:

1. Generate (subgraphs, supergraph, operation) as we do today.
2. Run both planners → two query plans (call them P_h, P_b).
3. Hand each plan to a deterministic mock-executor backend that
   resolves every subgraph fetch the plan asks for, returning a JSON
   object whose values are functions of the requested fields and the
   entity keys.
4. Compare the two response trees. If the plans are *equivalent* the
   responses must match byte-for-byte (modulo the same normalisation
   the plan diff uses).

This catches:
- Plans that look structurally different but are semantically the
  same (false-positives in the current diff layer — already largely
  normalised, but not perfectly).
- Plans that look structurally identical but go wrong at execution
  (currently invisible).
- Field-merging / type-conditioned-fetching bugs that only manifest
  in the response shape, not in the plan tree.

## Why it doesn't matter for the planner-speed work

The optimisation work is a *correctness-preserving* refactor of
plan production. As long as the new planner produces a plan that's
*plan-equivalent* to the old one (which the current diff layer
catches), the executor will produce the same response by definition.
Response-equivalence and plan-equivalence are equivalent on top of a
fixed executor. So the existing harness is sufficient for that
workflow.

Where response-equivalence becomes valuable is when *executor
behaviour* enters the picture — for instance, planner-executor
contract changes, response-merging fixes, or any optimisation that
deliberately changes the plan but claims to preserve responses.

## What it would take to build

This is the moderately-large piece. Three sub-systems:

### 1. Deterministic mock subgraph backend

For every entity type in the supergraph, the mock backend needs to
respond to:
- Root-field resolutions (`qT0: T0 { id ... }`)
- Entity-fetch resolutions (`_entities(representations: [...])`)
- Field selections including aliases, fragments, `@skip`/`@include`,
  `@defer`

The backend must be:
- **Deterministic**: same key + same field name → same value, every
  time, across both runs.
- **Seedable**: a sweep seed parameter so different ops produce
  different worlds.
- **Self-consistent**: if the same entity is fetched from two
  different subgraphs (federation's whole point), the response must
  match.

A reasonable design:
- Hash `(entity_type, entity_key_field_values, field_name, seed)`
  via SipHash → produce a deterministic value of the right type
  (scalar) or a lazily-resolved sub-object (entity).
- Cache results in a request-scoped map so repeated fetches of the
  same entity-field within a single execution return the same value.

### 2. Executor harness

Glue between a `QueryPlan` and the mock backend. Walks the plan,
issues fake subgraph requests, merges responses according to plan
node semantics (`Sequence`, `Parallel`, `Flatten`, `Defer`,
`Condition`), and produces a final response tree.

The router has an executor in `apollo-router/src/services/...` but
it's tightly coupled to real HTTP / runtime concerns. Easier to
write a minimal executor for this purpose — it only needs to handle
the plan node variants the test corpus produces.

### 3. Response diff layer

`response::normalize` + `response::diff` — same pattern as
`diff::normalize` for plans. Should canonicalise array ordering for
list fields, drop noise, and emit unified diffs for divergent
trees.

## Rough effort estimate

- Mock subgraph: ~300–500 LOC plus a small `arbitrary`-driven
  property test that confirms determinism.
- Executor harness: ~500–800 LOC depending on how many `QueryPlan`
  node variants we cover. Defer is its own complexity (multipart
  response stream).
- Response diff: ~150–250 LOC, similar style to `diff::normalize`.

Total: probably **1–1.5 days of focused work** to a usable v1, more
to cover the long tail of plan node variants and merge edge cases.

## Where it slots in

If this plan is picked up:

1. Build the mock + executor + response diff under
   `apollo-federation-fuzz/src/exec/`.
2. Add a new `DiffOutcome::ResponseDivergent { ... }` variant that
   captures plan-equivalent-but-response-divergent cases.
3. Wire `bin/fuzz` to optionally run response comparison alongside
   plan comparison (`--compare responses`).
4. Run a sweep — the question that comes back: how often do
   plan-equivalent plans produce different responses today? If the
   answer is "0 across 100k ops" the layer is over-engineering for
   this codebase. If it's nonzero, every instance is a genuine
   correctness finding the existing harness can't see.

The exploratory sweep is the experiment that decides whether the
layer is worth keeping.

## What this plan does *not* try to do

- Solve the planner-speed problem. That's separate; the existing
  plan-equivalence diff is sufficient for verifying correctness-
  preserving optimisations.
- Replace the planner-version side-by-side approach. Response
  comparison is *additive* — it's a second comparison layer on top
  of the same generated inputs.
- Implement a real federation executor. The goal is the smallest
  deterministic mock that's adequate for differential testing, not
  a router replacement.
