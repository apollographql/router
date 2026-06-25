# 04 — Report: Apollo Router startup memory scales O(S²) with the number of connectors

> Phase 4. Synthesis of `00`–`03` plus a draft `apollographql/router` issue.
> Branch `smyrick/6817694b` (shareable, not merged to main). **The GitHub issue below is a
> DRAFT — do not file until approved.**

## TL;DR

A Router supergraph built entirely from **Apollo Connectors** uses **dramatically** more
startup memory than an equivalent supergraph of plain subgraphs, and the gap grows
**super-linearly (≈O(S²))** with the number of `@connect` directives. A parametric local
reproduction shows the connectors family allocating **87× more at 384 connectors and 190×
more at 768 connectors** than a type-for-type identical pure-subgraph control, with peak
heap and startup time both growing quadratically. Root cause: connector expansion creates
**one synthetic subgraph per `@connect`**, and shared entities produce a near-complete
cross-subgraph key-edge graph (**O(S²) edges**); the federated query-graph builder then (a)
materializes a per-edge followup-edge `Vec` for all O(S²) edges and (b) re-sorts a fresh
edge `Vec` on **every** `out_edges()` call. This is the startup OOM the Constellation team
is hitting (14 GiB and climbing, all-connector graph).

## Evidence (scaling)

Parametric supergraphs, K=8 fields + E=4 shared entities per subgraph, swept over N
subgraphs. Connectors expand to S = N·(K+E) synthetic subgraphs; pure stays at N.
Full-router startup, dhat-heap + `/usr/bin/time -l`:

| N | @connect (=S) | connectors peak heap | pure peak heap | peak ratio | connectors total alloc | pure total alloc | total ratio | connectors RSS | startup |
|---|---|---|---|---|---|---|---|---|---|
| 8  | 96  | 39.6 MiB | 6.8 MiB | 5.8× | 191.8 MiB | 21.7 MiB | 8.8× | 682 MiB | 9.9 s |
| 16 | 192 | 102.9 MiB | 10.9 MiB | 9.5× | 875.7 MiB | 32.3 MiB | 27× | 751 MiB | 20.9 s |
| 32 | 384 | 391.6 MiB | 19.5 MiB | 20× | **5.42 GiB** | 64.2 MiB | **87×** | 1.05 GiB | 49 s |
| 64 | 768 | **2.13 GiB** | 42.4 MiB | 51× | **40.3 GiB** | 216.9 MiB | **190×** | **2.39 GiB** | **160 s** |

Scaling exponents (memory ∝ Sᵏ): connectors peak **1.65**, total **2.4–2.8**; pure **0.78**
(sub-linear). Federation-only isolation (no full router) gives the cleanest signal:
connectors planner peak exponent **2.02** (textbook O(S²)) vs pure **1.09**, and the
`connectors_N32` planner alone accounts for **98%** of full-router allocation — proving the
cost is the **federation planner/expansion**, not router infrastructure. The pure-subgraph
RSS is essentially flat (646→688 MiB) across the entire sweep.

The **87% memory cut** the customer saw by splitting connectors across 6 routers matches
the measured **87× allocation ratio** at N=32 — the same superlinear effect, viewed from
two directions.

## Root cause (dhat attribution → code)

`S` = number of synthetic subgraphs = number of `@connect` directives.

1. **Structural:** `expand_connectors` (`apollo-federation/src/connectors/expand/mod.rs:56`)
   creates one synthetic subgraph per `@connect`. Shared `@key` entities are resolvable from
   every synthetic subgraph, so the federated query graph forms a near-complete
   cross-subgraph KeyResolution edge set → **O(S²) edges** (dhat: 152,960 live blocks ≈
   384² at N32).
2. **Peak heap (53%):** `precompute_non_trivial_followup_edges`
   (`apollo-federation/src/query_graph/build_query_graph.rs:292`) stores a per-edge `Vec` of
   followup edges for all O(S²) edges → **218 MiB** at N32, growing **O(S²)**.
3. **Allocation churn (86%):** `out_edges()` (`apollo-federation/src/query_graph/mod.rs:702`)
   collects+sorts a **fresh `Vec` on every call** via `sorted_edges` (`mod.rs:726`), with no
   caching; called once per edge by (2) and throughout the build → **4.77 GiB** total
   allocated, the main driver of the 160 s startup and allocator/RSS pressure.
4. **Secondary (linear, router-side):** each synthetic subgraph builds its own
   `HttpClientService` that clones the rustls `RootCertStore`
   (`apollo-router/src/services/http/service.rs:359`) → ~30 MiB at N32 (O(S)).

The originally-suspected `schema_subtypes_map` per-connector clone, `handle_key`, and
`copy_subgraphs` are **near-zero at peak** — transient churn, not resident. `handle_key`
still matters as the *creator* of the O(S²) key edges, but the memory lives in the petgraph
edges + (2) + (3).

## Fix candidates (for router/federation engineers)

Ordered by expected impact / risk:

1. **Cache `out_edges` / followup-edge results** instead of re-sorting a fresh `Vec` on
   every call (`query_graph/mod.rs:702,726`). Likely removes ~the entire 4.77 GiB churn and
   most of the startup time. Low risk, localized.
2. **Don't eagerly materialize `precompute_non_trivial_followup_edges` for O(S²) edges**
   (`build_query_graph.rs:292`): compute lazily, or store followups compactly (indices /
   bitsets) instead of per-edge `Vec<EdgeReference>`. Removes ~53% of peak.
3. **Collapse the O(S²) key-edge explosion** — the deeper fix. Synthetic subgraphs that
   share a `@source` should not each be mutually key-resolvable; a source-aware planner (or
   grouping connectors per source subgraph instead of per `@connect`) cuts S and the edge
   count at the root. This is the long-term direction the expansion comment already
   anticipates ("until we have a source-aware query planner").
4. **`Arc`-share the rustls `RootCertStore`** across per-subgraph HTTP clients
   (`http/service.rs:359`) — secondary, linear.

## Standalone reproduction (a router engineer can run this)

Everything is on branch `smyrick/6817694b` under `investigation/connectors-startup-memory/`.

```bash
# 1. generate + compose both families (rover 0.40 installed; APOLLO_ELV2_LICENSE=accept)
bash scripts/compose_all.sh                      # -> artifacts/<family>_N<n>/supergraph.graphql + manifest.tsv

# 2. full-router startup memory sweep (needs dhat router build)
cargo build --profile release-dhat -p apollo-router --features dhat-heap
bash scripts/measure_all.sh                      # -> artifacts/measurements.tsv (+ per-run dhat-heap.json)
#    N64 connectors needs more startup headroom:
READY_ITERS=1400 bash scripts/measure_one.sh artifacts/connectors_N64/supergraph.graphql artifacts/connectors_N64/run

# 3. federation-only planner isolation (no full router)
bash scripts/fed_planner_all.sh                  # -> artifacts/planner_measurements.tsv

# 4. attribute a dhat profile
python3 scripts/parse_dhat.py artifacts/connectors_N32/run/dhat-heap.json --top 15
```

The federation-only test is `apollo-federation` integration test
`connectors_startup_profiling` (env-gated by `CONNECTORS_SUPERGRAPH`, `#[ignore]`d so it
never runs in normal `cargo test`).

Caveats: measured on macOS/dhat; the customer OOMs on Linux, but the **allocation shape** is
platform-independent (dhat attribution transfers; absolute RSS differs). The repro is
synthetic (parametric) — a real `constellation-registry` export would validate absolute
numbers but the scaling law is the point.

---

## DRAFT GitHub issue — `apollographql/router` (DO NOT FILE until approved)

**Title:** Startup memory scales ~O(S²) with the number of `@connect` directives (all-connector supergraphs OOM at startup)

**Labels:** `connectors`, `performance`, `memory`

**Body:**

### Summary

For supergraphs composed entirely (or largely) of Apollo Connectors, Router **startup**
memory grows **super-linearly — approximately O(S²)** in the number of `@connect`
directives (S = synthetic subgraphs). Large all-connector graphs OOM at startup (a
production deployment is at 14 GiB and still OOM-ing). An equivalent supergraph of plain
subgraphs with identical type/field/entity counts uses **flat, linear** memory.

### Reproduction

Parametric generator + measurement scripts: <link to branch/gist>. Two families at
parameter N (K=8 fields, E=4 shared entities per subgraph): a **connectors** family (each
`@connect` expands to its own synthetic subgraph, S = N·(K+E)) and a type-for-type identical
**pure-subgraph** control.

| N | @connect (S) | connectors peak heap | pure peak heap | connectors total alloc | pure total alloc | connectors RSS | startup |
|---|---|---|---|---|---|---|---|
| 8  | 96  | 39.6 MiB | 6.8 MiB | 191.8 MiB | 21.7 MiB | 682 MiB | 9.9 s |
| 32 | 384 | 391.6 MiB | 19.5 MiB | 5.42 GiB | 64.2 MiB | 1.05 GiB | 49 s |
| 64 | 768 | 2.13 GiB | 42.4 MiB | 40.3 GiB | 216.9 MiB | 2.39 GiB | 160 s |

Scaling exponents: connectors peak **1.65**, total alloc **2.4–2.8**; pure **0.78**.
Federation-only isolation (`expand_connectors` + `QueryPlanner::new`, no full router) gives
connectors planner peak exponent **2.02** vs pure **1.09**, and accounts for **98%** of
full-router allocation — so this is in the federation query planner, not router infra.

### Root cause (dhat heap attribution at S=384)

- Connector expansion creates **one synthetic subgraph per `@connect`**
  (`apollo-federation/src/connectors/expand/mod.rs:56`); shared entities make every synthetic
  subgraph mutually key-resolvable → a near-complete cross-subgraph KeyResolution graph =
  **O(S²) edges** (dhat: 152,960 live blocks ≈ 384²).
- **Peak (53%):** `precompute_non_trivial_followup_edges`
  (`apollo-federation/src/query_graph/build_query_graph.rs:292`) stores a per-edge `Vec` for
  all O(S²) edges → 218 MiB.
- **Churn (86% of 5.55 GiB):** `QueryGraph::out_edges`
  (`apollo-federation/src/query_graph/mod.rs:702`) collects+sorts a fresh `Vec` on **every**
  call (`sorted_edges`, `mod.rs:726`); no caching.
- **Secondary (linear):** per-synthetic-subgraph `HttpClientService::from_config_for_subgraph`
  clones the rustls `RootCertStore` (`apollo-router/src/services/http/service.rs:359`).

### Suggested fixes

1. Cache `out_edges`/followup-edge results instead of re-sorting per call (removes ~all
   churn, low risk).
2. Avoid eagerly materializing O(S²) per-edge followup `Vec`s (removes ~53% of peak).
3. Reduce the O(S²) key-edge explosion at the root — group synthetic subgraphs per `@source`
   rather than per `@connect` / source-aware planning.
4. `Arc`-share the rustls `RootCertStore` across per-subgraph HTTP clients.

### Environment

Reproduced with Router `2.15.0` (this worktree), federation `connect/v0.3` + `federation/v2.12`,
rover 0.40.0, macOS, dhat-heap profile. Customer impact on Linux; allocation shape is
platform-independent.
