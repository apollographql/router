# Bisection sweep: PR #7580 + FED-505 fix window

After Phases I + J saturated the 2.0.0 → 2.13.1 pair on PR #7580 and
FED-505, I swept three intermediate versions to locate when each fix
landed.

| BASE → HEAD (2.13.1)        | Iter / op | Divergences |
|------------------------------|-----------|-------------|
| `=2.0.0`  → 2.13.1           | 1000      | 87 – 110    |
| `=2.1.3`  → 2.13.1           | 1000      | **87**      |
| `=2.5.0`  → 2.13.1, seed=17  | 1000      | **0**       |
| `=2.5.0`  → 2.13.1, seed=42  | 1000      | 0           |
| `=2.5.0`  → 2.13.1, seed=1234| 1000      | 0           |
| `=2.5.0`  → 2.13.1, seed=99  | 1000      | 0           |
| `=2.13.0` → 2.13.1           | 1000      | 0           |

## Findings

1. **`2.1.3` still has both bug classes.** 87 divergences, distributed
   identically to 2.0.0 (39 PR #7580 only, 48 PR #7580 + Condition).
2. **`2.5.0` is clean.** Confirmed across 3 seeds × 1000 ops = 3000
   ops. So both PR #7580 (`... on Query` extraneous root condition) and
   FED-505 (missing `Condition` plan node for `@skip`/`@include`) were
   fixed in the **(2.1.3, 2.5.0]** window.
3. **`2.13.0` → `2.13.1` is byte-clean.** No plan-observable change
   between these point releases.

This narrows the upstream-actionable result: rather than just "the
harness rediscovers PR #7580 and FED-505 against 2.0.0", we can say
the fixes landed in a specific 4-version window. Anyone investigating
those PRs can git-bisect inside that window.

## What this implies for finding *new* bugs

The 2.5.x → 2.13.x window is converged on consistent plan output for
this generator's covered surface. To surface new bugs on this version
pair, the harness needs *new* surface — not more chances at the same
patterns. Likely high-value next moves:

- Add `@interfaceObject` (PR #8109 territory).
- Add `@provides` (different planner code path than `@requires`).
- Move from apollo-smith to a richer operation generator (deeper
  fragment nesting, named fragment definitions, more aliases).
- Add semantic comparison (issue same op against an executor backed by
  both planners; compare *responses*, not plans). This catches "both
  planners agree on a wrong answer" — currently invisible.

## Curated artifact

- `PR_7580_FED505_present_at_2.1.3.txt` — one of the 87 divergences
  showing both bug classes are still present at 2.1.3 (same diff shape
  as the 2.0.0 sweep).

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml — pick a base
apollo-federation-base = { package = "apollo-federation", version = "=2.5.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.5.0
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
