# Workflow: differential testing for planner optimisation work

This doc is the on-ramp for using the harness to validate planner
optimisations — i.e. you have a branch that changes how the planner
*works internally* but is supposed to be correctness-preserving and
faster. The harness exists to catch:

1. Subtle correctness regressions that only show up on pathological
   shapes (the entire point of the random-generation infra).
2. New panics on inputs the existing planner accepts.
3. Net plan changes that aren't supposed to be there.

Plus, eventually, performance A/B (separate effort, see "Perf A/B
notes" below).

## Setup

Pin the baseline you want to compare against in `Cargo.toml`:

```toml
# A. Compare against a published crates.io release:
apollo-federation-base = { package = "apollo-federation", version = "=2.13.0" }

# B. Compare against a specific git revision (e.g. current dev):
apollo-federation-base = { package = "apollo-federation", git = "https://github.com/apollographql/router", rev = "<sha>" }

# C. Compare against a sibling working copy (fastest iteration):
apollo-federation-base = { package = "apollo-federation", path = "../apollo-federation-baseline" }
```

`HEAD` is always the in-tree workspace member at
`apollo-federation/`, so your optimisation branch goes there.
`harness_base.rs` is written to be tolerant of API drift back to
`=2.0.0`, so older baselines work without harness changes.

## Correctness validation pass

The point of this pass is "did my optimisation change any plan that
shouldn't have changed?"

1. Run the package test suite first:

       cargo test -p apollo-federation-fuzz

   - `gen_compose` (~200 schemas / 200 ops): plan equivalence and
     composition. This is your fast smoke test (~15 s).
   - `regression_replay`: 3 curated reproducers from earlier
     phases. If your branch breaks plan output on PR #7580 / Class E
     / defer artefacts, these break first.
   - `determinism`: confirms seed → output is stable. If your
     optimisation accidentally introduces non-determinism (e.g.
     replaces a `BTreeSet` with a `HashSet`), this catches it.

2. Run a 1k-op sweep:

       cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
           --iterations 1000 --seed 17 --ops-per-schema 5 \
           --enable-defer \
           --regressions-dir /tmp/perf_correctness_check

   Expected vs an unmodified baseline: 0 plan divergences against
   any 2.5+ baseline (see `CROSS_VERSION_FINDINGS.md`). Any new
   divergence is something your optimisation changed.

3. **Categorise any divergences.** Same as the existing CR work —
   look at `=== DIFF ===` content. The 5 already-characterised
   classes (PR #7580, FED-505, defer-C, defer-D, alias reorder) all
   only fire against pre-2.5 baselines. Against a recent baseline,
   *any* divergence is a new finding to investigate.

4. Pressure-test at 10k:

       for seed in 17 42 99 1234 7; do
           cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
               --iterations 10000 --seed $seed --ops-per-schema 5 \
               --enable-defer \
               --regressions-dir /tmp/perf_check_seed_$seed
       done

   This is the same 50k-op corpus the panic-class tail-bug was
   characterised against. If your optimisation introduces a *new*
   panic class, it'll show up alongside the known
   `process_root_nodes` one (which exists pre-optimisation; see
   `cross_version_2.5.0_panic_10k/README.md` and the cross-ref to
   PR #9123/#9250).

   Acceptance criterion: **same number and class of panics as the
   unmodified baseline run**, no new divergences.

## Perf A/B notes (when this gets built)

Earlier exploration flagged real concerns to design around. Saving
them here so they're not lost when the perf layer lands:

- **Cache/order effects.** Running HEAD before BASE every iteration
  biases timing — the second-run side benefits from warm caches
  (allocator, instruction cache, branch predictor). Mitigation:
  randomise the head/base order per op (a coin flip on the seed).
- **Single-call wall-clock noise.** Plan times at sub-millisecond
  scale are dominated by jitter, not the planner. Mitigation:
  - run each (schema, op) pair `N` times per side and use the
    median of the N as that pair's measurement, OR
  - hunt outliers (≥ 2x ratios) rather than 5% regressions, OR
  - add complementary noise-free metrics (plan node count, fetch
    count, JSON size, depth) which can catch plan-shape changes
    that imply perf changes without depending on the clock.
- **What "regression" means semantically.** Sometimes HEAD is
  legitimately slower because it's doing *correct* extra work that
  BASE was skipping (the FED-505 `Condition` node, the defer
  `sub_selection` field). The harness can't tell those apart from
  real regressions without human triage. Plan-shape metrics also
  help here — "HEAD has 3x more nodes than BASE on this op" is at
  least a precise question.
- **Two-version perf comparison ≠ absolute perf measurement.**
  The harness compares two compiled-in planners running in the same
  process. Useful for "did my optimisation help?" — not useful for
  "is the planner fast enough?". For absolute numbers use
  `apollo-router-benchmarks/`.

The lowest-noise starting point would be plan-shape metrics, not
wall-clock — and we already have everything needed for that
(serialise both plans → count nodes / measure depth / measure JSON
size). Wall-clock layering on top is straightforward but optional.

## Pointers

- `CROSS_VERSION_FINDINGS.md` — what divergence classes exist and
  in what version windows. Read first to know what "expected"
  divergence looks like.
- `COVERAGE_GAPS.md` — historical bug surface index, mostly closed.
- `OP_GEN_IMPROVEMENT_PLAN.md` — saved plan for richer op generation
  beyond apollo-smith. Phase A is shipped; Phase B/C deferred.
- `RESPONSE_COMPARISON_PLAN.md` — saved plan for executor-backed
  response-equivalence comparison. Not needed for the planner-
  optimisation workflow above (plan equivalence is sufficient for
  correctness-preserving refactors), but kept as a future option.
- `examples/minimize_panic.rs` — hand-minimised reproducer for the
  one known HEAD panic; useful template if a new finding needs
  shrinking before reporting upstream.
