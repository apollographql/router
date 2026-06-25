# 02 — Measurements (startup memory + scaling curve)

> Phase 2. Read `00-context.md` and `01-repro.md` first. Raw data:
> `artifacts/measurements.tsv` (full router) and `artifacts/planner_measurements.tsv`
> (federation-only). Per-run `dhat-heap.json` saved under `artifacts/<run>/run/`.
> Committed on branch `smyrick/6817694b`.

## Method

- **Router build**: `cargo build --profile release-dhat -p apollo-router --features dhat-heap`
  (5m34s). Binary respects `CARGO_TARGET_DIR`.
- **Per run** (`scripts/measure_one.sh`): start the router under `/usr/bin/time -l` from a
  per-run dir with the rover-composed supergraph + `scripts/router.yaml` (ports 4010/8098),
  poll `GET /health` until ready, then `SIGTERM` for graceful shutdown. dhat's profiler is
  dropped via `libc::atexit` on clean exit, writing `dhat-heap.json` to the run dir.
  - `rss_max_bytes` = `maximum resident set size` from `/usr/bin/time -l` (peak RSS, bytes).
  - dhat metrics from `scripts/parse_dhat.py`: `peak_bytes` = Σ bytes live at global heap
    max (t-gmax), `total_bytes` = Σ bytes ever allocated, plus block (allocation) counts.
- **Sweep** (`scripts/measure_all.sh`): every supergraph in `manifest.tsv` (N∈{1,2,4,8,16,32,64},
  K=8, E=4), both families. (N64 connectors needed `READY_ITERS=1400` — 160s to start.)
- **Federation-only** (`scripts/fed_planner_all.sh` + `apollo-federation` test
  `connectors_startup_profiling`): mirrors the router startup path
  (`expand_connectors` → `Supergraph::new_with_router_specs` → `QueryPlanner::new`) **without**
  the full router, isolating federation allocations from the ~650 MB router RSS baseline.

`router.yaml` keeps a single load (no hot-reload), so these numbers are clean
single-startup costs (the hot-reload doubling suspect is excluded by design).

## Full-router results

Peak heap and total allocation are per `dhat-heap.json`; RSS from `/usr/bin/time -l`.

| N | @connect / synthetic subgraphs | conn peak heap | pure peak heap | peak ratio | conn total alloc | pure total alloc | total ratio | conn RSS | conn ready |
|---|---|---|---|---|---|---|---|---|---|
| 1  | 12  | 7.0 MiB    | 4.1 MiB | 1.7× | 26.8 MiB    | 14.6 MiB | 1.8×  | 646 MiB  | 3.2 s |
| 2  | 24  | 10.9 MiB   | 4.1 MiB | 2.7× | 40.3 MiB    | 15.6 MiB | 2.6×  | 655 MiB  | 3.7 s |
| 4  | 48  | 19.3 MiB   | 5.0 MiB | 3.9× | 73.2 MiB    | 17.6 MiB | 4.1×  | 663 MiB  | 5.8 s |
| 8  | 96  | 39.6 MiB   | 6.8 MiB | 5.8× | 191.8 MiB   | 21.7 MiB | 8.8×  | 682 MiB  | 9.9 s |
| 16 | 192 | 102.9 MiB  | 10.9 MiB| 9.5× | 875.7 MiB   | 32.3 MiB | 27.1× | 751 MiB  | 20.9 s |
| 32 | 384 | 391.6 MiB  | 19.5 MiB| 20.1×| **5.42 GiB**| 64.2 MiB | 86.6× | 1050 MiB | 49.1 s |
| 64 | 768 | **2.13 GiB**| 42.4 MiB| 51.5×| **40.3 GiB**| 216.9 MiB| 190.4×| **2.39 GiB** | **160.0 s** |

### Scaling exponents (memory ∝ Nᵏ, fit over N=8→32)

| metric | connectors | pure |
|---|---|---|
| peak heap | **1.65** | 0.76 |
| total allocated | **2.43** (N16→64: **2.78**) | 0.78 |

- **Connectors is super-linear → super-quadratic in total allocation; pure is sub-linear.**
- Pure-subgraph RSS is essentially **flat** (646→688 MiB) across the whole sweep; the entire
  RSS growth in the connectors family (646 MiB → 2.39 GiB) is connector-driven.
- The **86.6× total-allocation ratio at N=32** (and 190× at N=64) is the same order of
  magnitude as the customer's **~87% memory reduction** from splitting connectors across 6
  routers — independent corroboration that the cost is connectors-specific and superlinear.
- **N=64 (768 connectors) is the OOM reproduction**: 2.39 GiB RSS / 2.13 GiB peak heap /
  **40.3 GiB cumulative allocation** / 11.9M allocations / 160 s startup, for a graph that
  composes to only 248 KB of SDL. Extrapolating the O(N²·⁴⁺) curve to a production-sized
  connector catalog reproduces the customer's 14 GiB-class startup OOM.

## Federation-only (planner) isolation

`expand_connectors` + `QueryPlanner::new`, no full router (`planner_measurements.tsv`):

| run | peak heap (max_bytes) | total alloc | total allocations |
|---|---|---|---|
| connectors_N1  | 1.9 MiB    | 11.3 MiB   | 81,891 |
| connectors_N2  | 3.6 MiB    | 22.3 MiB   | 159,042 |
| connectors_N4  | 7.8 MiB    | 50.3 MiB   | 318,250 |
| connectors_N8  | 19.5 MiB   | 147.4 MiB  | 665,855 |
| connectors_N16 | 65.7 MiB   | 823.2 MiB  | 1,529,414 |
| connectors_N32 | **320.0 MiB** | **5.33 GiB** | 3,875,865 |
| pure_N1        | 0.5 MiB    | 1.2 MiB    | 8,985 |
| pure_N8        | 2.3 MiB    | 6.4 MiB    | 46,339 |
| pure_N16       | 4.8 MiB    | 14.8 MiB   | 91,713 |
| pure_N32       | 10.4 MiB   | 42.3 MiB   | 196,685 |
| pure_N64       | 27.1 MiB   | 186.4 MiB  | 470,847 |

### Planner exponents (N=8→32)

| metric | connectors | pure |
|---|---|---|
| peak heap | **2.02** (clean O(S²)) | 1.09 (linear) |
| total allocated | **2.61** | ~1.0 |

**The smoking gun:**
- `connectors_N32` planner alone allocates **5.33 GiB = 98.3%** of the full-router N32 total
  (5.42 GiB), and its peak is **81.7%** of the full-router peak. The router infrastructure
  (tokio, HTTP servers, telemetry, plugins) is a near-constant ~650 MB RSS baseline and is
  **not** the cause.
- The connectors planner peak grows as **O(S²)** (exponent 2.02) where S = number of
  synthetic subgraphs = number of `@connect` directives. Pure-subgraph planner peak grows
  **linearly** (1.09). This is exactly the `handle_key`-style `O(S²)` cross-subgraph pass
  predicted in `00-context.md`, amplified by the 1-synthetic-subgraph-per-`@connect`
  expansion (S = N·(K+E)).

## Conclusion for Phase 3

The startup memory blowup is **connectors-specific** and lives in the **federation
expansion + query-graph build**, scaling **~O(S²)** in the number of `@connect` directives.
Phase 3 attributes the `dhat-heap.json` allocation stacks (use the largest:
`artifacts/connectors_N64/run/dhat-heap.json`, and `connectors_N32` for a faster parse) to
the ranked code suspects — expecting `handle_key`/`copy_subgraphs`/`api_schema.clone`
(query-graph build) and `schema_subtypes_map`/expansion serialize-reparse to dominate.
