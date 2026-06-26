# HANDOFF — Connectors startup memory investigation

> For the next agent/engineer picking this up. Self-contained entry point.
> Branch: `smyrick/6817694b` (pushed to `origin`, **not** merged to main).
> GitHub: https://github.com/apollographql/router/tree/smyrick/6817694b

## 1. What this is

Investigation into Apollo Router using huge memory with all-connector supergraphs
(Constellation/GraphOS-for-Agents team; 14 GiB and OOM-ing). We built a parametric
reproduction, profiled it (dhat-heap + RSS), root-caused the allocation hotspots, and
posted findings + reviewed Ben Newman's fix PR. All artifacts are committed under
`investigation/connectors-startup-memory/`.

## 2. Read these first (in order)

- `00-context.md` — Slack thread facts, hypothesis, verified code suspects (file:line).
- `01-repro.md` — parametric generator design, exact commands, validation gate.
- `02-measurements.md` — full scaling tables (connectors vs pure control), exponents.
- `03-rootcause.md` — dhat attribution → code (**corrects** the 00 hypothesis).
- `04-report.md` — synthesis + fix candidates + **draft GitHub issue (not filed)**.

## 3. Bottom line (TL;DR)

- Connector expansion makes **1 synthetic subgraph per `@connect`** (S = N·(K+E)). Shared
  entities ⇒ near-complete cross-subgraph KeyResolution graph ⇒ **O(S²) edges**.
- Router **startup** memory scales **~O(S²)** (peak-heap exp 1.65, total-alloc 2.4–2.8) vs a
  type-identical pure-subgraph control which is **sub-linear (0.78)**. At 768 connectors:
  **2.4 GiB RSS / 2.1 GiB peak heap / 40 GiB allocated / 160s startup**; 87× the pure
  control's allocation at N=32 (mirrors customer's ~87% cut from splitting routers).
- **Peak driver (53%)**: `precompute_non_trivial_followup_edges`
  (`apollo-federation/src/query_graph/build_query_graph.rs:292`) — per-edge `Vec` over O(S²) edges.
- **Churn driver (86% of 5.5 GiB)**: `QueryGraph::out_edges`→`sorted_edges`
  (`apollo-federation/src/query_graph/mod.rs:702,726`) re-sorts a fresh `Vec` every call.
- **Secondary (linear)**: per-synthetic-subgraph rustls `RootCertStore` clone
  (`apollo-router/src/services/http/service.rs:359`).
- The 00-hypothesis suspects (`schema_subtypes_map`, `handle_key`, `copy_subgraphs`) are
  **near-zero at peak** — transient churn, not resident. `handle_key` still *creates* the
  O(S²) edges.

## 4. ⚠️ The key open question (most important)

There are **two separate super-linear explosions over the same expansion**, both building a
`build_federated_query_graph` ("node per type × subgraph"):

1. **Composition-time satisfiability** (`apollo-federation/src/composition`, run by
   rover/GraphOS/federation-cli) — **Ben's PR #9663 fixes this** (14 GB → 0.3 GB).
2. **Router-process startup** `expand_connectors` → `QueryPlanner::new` →
   `build_federated_query_graph` — what our dhat measured. **Untouched by #9663.** The router
   never runs satisfiability (verified: no `validate_satisfiability` in `apollo-router/src`).

**UNRESOLVED:** does the customer's 14 GB land in (1) GraphOS/compose when onlining services,
or (2) the router process at startup? 
- If (1): #9663 is the fix — verify and close.
- If (2): #9663 won't help on its own (router builds the full O(S²) query graph from the
  un-consolidated executed schema; you can't feed it the consolidated schema — connectors
  need distinct subgraphs to execute).

This was raised with Ben in Slack (see §7). **Get this answer before deciding next steps.**

## 5. Ben Newman's PR #9663 (status: OPEN, reviewed by us)

`feat(connectors): consolidate synthetic subgraphs to improve satisfiability performance`.
Merges join graphs with identical `(resolvable-key signature, link scope)` into one
representative, **only for the satisfiability check** (executed schema untouched). Approach
is sound (reachability-monotonic; merge can only mask, never invent an error; merging
key-identical graphs preserves the verdict; broken-test guards false-pass). Our review:
solid fix for composition cost; scope gap is the router-startup path (§4).

## 6. How to resume the repro (everything scripted)

Prereqs: rover **0.40.0** (`npm i -g @apollo/rover@latest`), `APOLLO_ELV2_LICENSE=accept`,
`cargo`. Note `CARGO_TARGET_DIR` may be redirected (scripts honor it / `ROUTER_BIN`).

```bash
cd investigation/connectors-startup-memory

# generate + compose both families (writes artifacts/<family>_N<n>/ + manifest.tsv)
bash scripts/compose_all.sh

# full-router startup sweep (needs dhat router build, ~5.5min)
cargo build --profile release-dhat -p apollo-router --features dhat-heap
bash scripts/measure_all.sh                 # -> artifacts/measurements.tsv
# N64 needs more startup headroom (160s):
READY_ITERS=1400 bash scripts/measure_one.sh artifacts/connectors_N64/supergraph.graphql artifacts/connectors_N64/run

# federation-only planner isolation (no full router)
bash scripts/fed_planner_all.sh             # -> artifacts/planner_measurements.tsv

# attribute a dhat profile
python3 scripts/parse_dhat.py artifacts/connectors_N32/run/dhat-heap.json --top 15
```

Knobs: `gen_schema.py --n/--k/--e`; `N_SWEEP`, `K`, `E` env vars for `compose_all.sh`.
`dhat-heap.json` files (~229 MB total) are **gitignored** — regenerate via `measure_all.sh`.
Federation-only test: `apollo-federation` integration test `connectors_startup_profiling`
(env-gated by `CONNECTORS_SUPERGRAPH`, `#[ignore]`d — won't run in normal `cargo test`).

## 7. What's been communicated

- Findings summary posted to Slack `#help`-adjacent thread (channel `C02UX05LF4K`, thread
  `1782407290.487089`): https://apollograph.slack.com/archives/C02UX05LF4K/p1782411812347319
- Review of #9663 posted in same thread:
  https://apollograph.slack.com/archives/C02UX05LF4K/p1782424817986089
- Branch pushed to GitHub (§1).

## 8. Suggested next actions

1. **Resolve §4** — confirm with Ben/Matt (Matt back July 6) / the customer whether the OOM
   is compose vs router startup. Drives everything else.
2. If router-startup matters: prototype the two independent optimizations and re-run
   `measure_all.sh` to quantify:
   - cache `out_edges`/sorted followups instead of re-sorting per call (`query_graph/mod.rs:702,726`);
   - lazy/compact `precompute_non_trivial_followup_edges` (`build_query_graph.rs:292`).
3. Optionally **file the drafted Router issue** (body in `04-report.md`) — held off since Ben
   already owns the composition half. Ask before filing to avoid a duplicate.
4. Validate against a **real** `constellation-registry` export (synthetic repro proves the
   scaling law; a real export confirms absolute numbers) — needs catalog access.
5. (Optional) run #9663's branch through `scripts/measure_all.sh` to empirically show whether
   it moves the router-startup curve (expectation: it won't, by construction).

## 9. Testing Ben's PR #9663 against our local tests (NEXT AGENT: do this)

Goal: run our repro/tests with Ben's changes applied and see what moves.

**Critical gotchas — read before running:**
- `compose_all.sh` uses **rover**, which downloads its **own bundled** supergraph plugin
  (v2.12.0) — it does **NOT** use local `apollo-federation`, so it will **not** reflect
  #9663. Use it only to *generate* the supergraph inputs.
- The **router startup** path (`measure_all.sh`, `fed_planner_all.sh`) does **not** run
  satisfiability, so it is expected to be **unchanged** by #9663 (this is the scope gap in
  §4 — confirming "no change here" is itself a useful result).
- #9663 lands in `validate_satisfiability_with_connectors`. The way to actually exercise it
  locally is the **federation-cli `satisfiability` subcommand**, which calls that function.
  Our generated schemas are a good trigger: the K item-connectors per subgraph are keyless
  (empty key-signature → all interchangeable) and the E entity-connectors group by shared
  `@key` — so consolidation should fire strongly.

### Setup: combine the PR with our repro (use two worktrees for clean before/after)

```bash
# repo root
git fetch origin benjamn/connector-satisfiability-collapse smyrick/6817694b

# BASELINE worktree (our branch, no PR)
git worktree add ../wkfz-base smyrick/6817694b

# WITH-PR worktree: our repro + Ben's changes merged (files are disjoint → clean merge)
git worktree add -b smyrick/6817694b-with-9663 ../wkfz-pr smyrick/6817694b
cd ../wkfz-pr && git merge --no-edit origin/benjamn/connector-satisfiability-collapse && cd -
```

### Test A — satisfiability path (this is where #9663 should win)

Run in BOTH worktrees and compare peak RSS + wall time. The supergraph inputs already exist
under `investigation/connectors-startup-memory/artifacts/` on our branch (committed); copy
or regenerate them in each worktree first (`bash .../scripts/compose_all.sh`).

```bash
# in each worktree (../wkfz-base and ../wkfz-pr):
for n in 16 32 64; do
  sg=investigation/connectors-startup-memory/artifacts/connectors_N$n/supergraph.graphql
  echo "== N$n =="
  /usr/bin/time -l cargo run --release -q -p apollo-federation-cli -- satisfiability "$sg" 2>&1 \
    | grep -E "maximum resident|SUCCESS|error|real"
done
```

Expectation: baseline grows steeply (multi-GB / many seconds by N64); WITH-PR drops sharply
(consolidation collapses interchangeable synthetic subgraphs) with an **identical pass/fail
verdict**. Record both into a new `artifacts/pr9663_satisfiability.tsv` and note the ratio.
(For allocator-accurate numbers instead of RSS, the `apollo-federation` dhat test
`connectors_validation_profiling` profiles the satisfiability path, but it uses a hardcoded
fixture — the `satisfiability` subcommand above against our N-sweep is the parametric test.)

### Test B — router-startup path (expected unchanged; confirms the scope gap)

```bash
# in ../wkfz-pr (PR applied):
cargo build --profile release-dhat -p apollo-router --features dhat-heap
bash investigation/connectors-startup-memory/scripts/measure_all.sh
# diff the result against our committed baseline:
git -C . diff --no-index ../wkfz-base/investigation/connectors-startup-memory/artifacts/measurements.tsv \
  investigation/connectors-startup-memory/artifacts/measurements.tsv || true
```

Expectation: **no meaningful change** vs the committed `measurements.tsv` — the router still
builds the full O(S²) query graph (`QueryPlanner::new`) from the un-consolidated executed
schema. If it *does* change, that's a surprise worth digging into.

### Report

Summarize Test A (the win) + Test B (no change) and tie back to §4's open question: A proves
#9663 fixes the composition/satisfiability cost; B shows the router-startup half is still
open. Post back to the Slack thread (§7) and/or this doc.

### Results (2026-06-25, this agent) — #9663 tested locally

Setup: baseline = `smyrick/6817694b`; with_pr = same + `origin/benjamn/connector-satisfiability-collapse`
merged into `smyrick/6817694b-with-9663` (worktree `../wkfz-pr`; one trivial `Cargo.toml`
conflict — both sides kept). Caveat: Ben's branch carries an unrelated `dev` merge, so the
with_pr tree differs from baseline by more than just the consolidation (matters only for the
sub-1% Test-B deltas, not Test A).

**Test A — satisfiability (`apollo-federation-cli satisfiability`, /usr/bin/time -l). PR WINS.**
Verdict identical (`[SUCCESS]`) for every N. Raw: `artifacts/pr9663_satisfiability.tsv`.

| N  | baseline RSS | PR RSS  | baseline real | PR real | RSS× | time× |
|----|--------------|---------|---------------|---------|------|-------|
| 16 | 99 MB        | 40 MB   | 0.70s         | 0.61s   | 2.5  | 1.1   |
| 32 | 416 MB       | 69 MB   | 4.22s         | 0.13s   | 6.0  | 32    |
| 64 | 2616 MB      | 127 MB  | 42.69s        | 0.26s   | 20.6 | 164   |

Baseline is steeply super-linear (matches §3); PR is near-flat. Consolidation collapses the
interchangeable synthetic subgraphs as designed. Confirms #9663 fixes the composition half.

**Test B — router startup (release-dhat router). UNCHANGED, as predicted (§4 scope gap).**
PR vs committed `measurements.tsv` baseline (N16/N32; N64's 160s startup skipped). Raw:
`artifacts/pr9663_router_startup.tsv`.

| N  | metric    | baseline   | PR         | Δ      |
|----|-----------|------------|------------|--------|
| 16 | dhat_peak | 107.93 MB  | 108.29 MB  | +0.3%  |
| 16 | rss_max   | 787 MB     | 795 MB     | +1.0%  |
| 32 | dhat_peak | 410.60 MB  | 411.24 MB  | +0.2%  |
| 32 | rss_max   | 1101 MB    | 1111 MB    | +0.9%  |

All within run-to-run noise (<1.3%, attributable to the dev-merge drift). The router never
calls satisfiability, so #9663 cannot move this curve — empirically confirmed.

**Bottom line:** #9663 is a decisive fix for the composition/satisfiability path (~20× RSS,
~164× wall at N64, same verdict) and does **nothing** for the router-startup O(S²) query-graph
build. So §4's open question still gates next steps: if the customer's 14 GB OOM is in
GraphOS/compose → #9663 is the fix; if it's in the router process at startup → still open
(needs the `query_graph/mod.rs` + `build_query_graph.rs` optimizations from §8.2).

## 10. Caveats

- Profiled on macOS/dhat; customer OOMs on Linux. Allocation **shape** is
  platform-independent (dhat attribution transfers); absolute RSS differs.
- Repro is synthetic/parametric by design (controls scaling); not the customer's real graph.
- The apollo-connectors skill-ref fetch was blocked (network); generator is grounded in this
  repo's validated connector fixtures instead (stronger truth for this worktree) — see `01-repro.md`.
