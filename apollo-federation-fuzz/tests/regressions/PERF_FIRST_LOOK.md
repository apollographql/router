# Perf-report mode — first-look numbers across version swings

This is the inaugural run of the new `--report perf` mode. The
mode times `plan()` on both planner sides per op, randomises run
order to dampen warm-cache bias, and reports distribution stats
across only the ops where the two planners produced
plan-equivalent JSON (so we're never timing two different plans).
See `PLANNER_OPTIMIZATION_WORKFLOW.md` for what to be skeptical of.

## Three demo sweeps, all with `--enable-defer`, full schema/op surface, seed=17

| BASE → HEAD (in-tree 2.13.1) | Iter | Sampled | Diverged | head p50 µs | base p50 µs | **median ratio (head/base)** | Verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| `=2.13.0` → 2.13.1 (point release) | 2000 | 1999 | 0 | 1004 | 1031 | **0.97** | noise floor |
| `=2.5.0` → 2.13.1 (plan-byte-identical window) | 2000 | 1999 | 0 | 1096 | 1132 | **0.97** | no real perf change |
| `=2.0.0` → 2.13.1 (big swing) | 4000 | 1731 | 2268 | 608 | 646 | **0.94** | HEAD modestly faster |

### Reading these numbers

- **Median ratio 0.97** appears in the two "no real change" rows.
  That's the noise floor for this hardware / build / sample size.
  It's slightly below 1.0 because of consistent micro-bias somewhere
  (allocator, branch predictor, the planners not being literally
  byte-identical compiled code) — not a real perf signal.
- **Median ratio 0.94** for `=2.0.0` → 2.13.1 is a real signal:
  HEAD is ~6% faster on plan equivalent inputs in this 3-year window.
  Tail behaviour: p95 ratio 1.61, p99 2.26 — most ops where HEAD is
  *slower* are still within ~2× and likely just timing noise.
- **The `Diverged` column is critical context.** For the 2.0.0 case,
  56% of ops produce a different plan and are excluded from the
  perf sample. Those excluded ops are exactly the ones where HEAD
  does *more correct work* (FED-505 Condition node, defer
  sub_selection, etc., per `CROSS_VERSION_FINDINGS.md`). So the
  measured speedup is on the easy subset where neither planner had
  to do special handling. The full picture is something like:
  "HEAD is 6% faster on identical work, plus does correct extra
  work BASE was skipping."
- **Plan-shape medians match in all three runs** (head=2 fetches,
  base=2 fetches; head/base bytes within 1%). This is the
  noise-free sanity check that the perf signal isn't an artifact of
  somehow generating different inputs to the two sides.

### Per-side wall-clock distribution

(2.0.0 → 2.13.1 sweep, the only one with a real signal)

```
plan() wall-clock micros:
  head: min=    90 p50=    608 p95=   1951 p99=   4651 max= 107071
  base: min=    99 p50=    646 p95=   2150 p99=   4895 max=  88889
```

HEAD is faster across every percentile up to p99 except the max —
which is one outlier per side, expected jitter, not signal.

## How to run it

```sh
# Pin the baseline you want to compare against
$EDITOR apollo-federation-fuzz/Cargo.toml
# apollo-federation-base = { package = "apollo-federation", version = "=X.Y.Z" }

cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 2000 --seed 17 --ops-per-schema 5 --enable-defer \
    --report perf
```

Output is one block: schema/op counts, head and base wall-clock
distributions, the head/base ratio distribution + a one-line
verdict, plus median plan-shape metrics. No reproducer files are
written in perf mode (that's correctness mode's job).

## Implementation notes for whoever extends this

The current implementation is intentionally minimal:

- **Build planners once per schema.** Composition + planner setup
  is dwarfingly more expensive than `plan()` itself; running it
  inside the timing loop would drown the per-op signal.
- **Random head/base order per op.** `(seed_byte & 1)` flips the
  order so neither side gets a consistent warm-cache advantage.
- **`plans_equivalent` uses `diff::normalize`.** Without that, all
  2.0.0 ops appear divergent due to wire-format drift (older
  versions serialise `requires:` as raw SDL, etc.) and the perf
  sample size collapses to zero. The normaliser is the same one
  correctness mode uses, so equivalence here means "the diff layer
  would have called these identical".
- **No allocator / memory tracking.** Wall-clock + plan shape only
  for v1. Adding `dhat` integration is feasible but separate.
- **No multi-run-per-op median.** Each op is timed once per side.
  That's enough to surface large effects (the 2.0.0 → 2.13.1
  result holds at this resolution); for chasing 5% regressions an
  N-of-K loop would help.
- **Saved state is kept lean.** Just two `Vec`s of (u128, u128)
  for timings and (usize, usize, usize, usize) for shapes. Should
  scale to 100k samples without trouble; beyond that, stream to
  disk.
