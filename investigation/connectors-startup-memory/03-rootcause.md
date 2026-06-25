# 03 — Root cause (dhat attribution → code)

> Phase 3. Read `00`/`01`/`02` first. Attribution parsed from
> `artifacts/connectors_N32/run/dhat-heap.json` (S=384 synthetic subgraphs) via
> `scripts/parse_dhat.py`. Committed on branch `smyrick/6817694b`.
>
> **This phase corrects the Phase-0 hypothesis.** The dominant *resident* cost is the
> **federated query-graph edge structures**, not `schema_subtypes_map`. The originally
> ranked suspects (`schema_subtypes_map`, `handle_key`, `copy_subgraphs`) are near-zero at
> peak — they are transient churn, not peak-resident.

## The structural amplifier (why connectors are O(S²))

Connector expansion produces **S = N·(K+E) synthetic subgraphs**, one per `@connect`
(`expand_connectors`, `connectors/expand/mod.rs:56`). Every shared `@key` entity is declared
in (and resolvable from) **all** S synthetic subgraphs, so the federated query graph gets a
near-**complete graph of cross-subgraph KeyResolution edges** among the entity nodes →
**O(S²) edges**. Pure subgraphs have S=N and few cross-edges → linear. dhat confirms the
scale: the dominant structure has **152,960 live blocks ≈ 384² = S²**.

## Peak heap attribution (N32: 391.6 MiB peak)

Ranked by bytes-alive-at-global-max (the OOM-relevant figure):

| rank | bytes @ peak | % of peak | site |
|---|---|---|---|
| 1 | **218.5 MiB** | **53%** | `precompute_non_trivial_followup_edges` (`build_query_graph.rs:297`) |
| 2 | 40.0 MiB | 10% | petgraph edge store via `BaseQueryGraphBuilder::add_edge` (`build_query_graph.rs:204`) |
| 3 | ~30 MiB | 8% | per-synthetic-subgraph HTTP/TLS: `HttpClientService::from_config_for_subgraph` clones the rustls `RootCertStore` (`apollo-router/src/services/http/service.rs:359`), 62,592 blocks |
| 4 | ~16 MiB | 4% | `extract_subgraphs_from_supergraph` + `coerce_schema_default_values` (`supergraph/mod.rs:360`, `compat.rs:314`), 384 blocks (per synthetic subgraph) |

**#1 is the smoking gun.** `precompute_non_trivial_followup_edges` iterates **every edge**
of the query graph and stores a per-edge `Vec` of its non-trivial followup edges:

```291:297:apollo-federation/src/query_graph/build_query_graph.rs
    /// Precompute which followup edges for a given edge are non-trivial.
    fn precompute_non_trivial_followup_edges(&mut self) -> Result<(), FederationError> {
        for edge in self.query_graph.graph.edge_indices() {
            let edge_weight = self.query_graph.edge_weight(edge)?;
            let (_, tail) = self.query_graph.edge_endpoints(edge)?;
            let out_edges = self.query_graph.out_edges(tail);
            let mut non_trivial_followups = Vec::with_capacity(out_edges.len());
```

With O(S²) edges this materializes O(S²) `Vec`s = 218 MiB at S=384, growing as **O(S²)**
(matches the planner peak exponent **2.02** in `02-measurements.md`).

## Allocation-churn attribution (N32: 5.55 GiB total allocated)

Ranked by total bytes ever allocated (drives startup *time* — 160 s at N64 — and allocator
pressure / RSS fragmentation under a real allocator):

| total alloc | allocations | site |
|---|---|---|
| **3,464 MiB** | 1,205,248 | `QueryGraph::sorted_edges` (`query_graph/mod.rs:726`) |
| **1,307 MiB** | 147,840 | `QueryGraph::sorted_edges` (second call site) |
| 218 MiB | 152,960 | `precompute_non_trivial_followup_edges` (same as peak #1) |
| 80 MiB | 17 | petgraph `add_edge` regrowth |
| ~66 MiB | — | `FederatedSchema` position maps (`schema/position`) |

**`sorted_edges` = 4.77 GiB = 86% of all allocation.** `out_edges(node)` builds a fresh
sorted `Vec` of a node's outgoing edges on **every call**, with no caching:

```702:730:apollo-federation/src/query_graph/mod.rs
    pub(crate) fn out_edges(&self, node: NodeIndex) -> Vec<EdgeReference<'_, QueryGraphEdge>> {
        Self::sorted_edges(self.graph.edges_directed(node, Direction::Outgoing).filter(
            // ...
        ))
    }
    // ...
    fn sorted_edges<'graph>(
        edges: impl Iterator<Item = EdgeReference<'graph, QueryGraphEdge>>,
    ) -> Vec<EdgeReference<'graph, QueryGraphEdge>> {
        let mut edges: Vec<_> = edges.collect();
        edges.sort_by_key(|e| -> EdgeIndex { e.id() });
        edges
    }
```

`precompute_non_trivial_followup_edges` calls `out_edges(tail)` once per edge
(`build_query_graph.rs:296`); over O(S²) edges, plus other graph traversals during the
build, this allocate-collect-sort churns multiple GiB.

## Verdict on the Phase-0 ranked suspects

| suspect (Phase 0) | bytes @ peak (N32) | verdict |
|---|---|---|
| `schema_subtypes_map` / `subtypes_map_from_schema` | ~0.1 MiB | **not** a peak driver; transient |
| `handle_key` (own allocations) | ~0.03 MiB | creates the O(S²) key edges (time cost) but its own allocations are tiny; edges live in petgraph |
| `copy_subgraphs` | ~0.01 MiB | negligible |
| `expand_connectors` (serialize/reparse) | ~0.5 MiB resident | small at peak; transient churn only |
| query-graph edge structures | **258 MiB (#1+#2)** | **the actual peak driver** |
| `out_edges`/`sorted_edges` churn | 4.77 GiB total | **the actual churn driver** |

`handle_key` is still relevant: it is what **creates** the O(S²) KeyResolution edges that
everything downstream pays for. But the memory lives in the petgraph edges +
`precompute_non_trivial_followup_edges` + repeated `out_edges` sorting, not in `handle_key`
or `schema_subtypes_map`.

## Complexity summary

| level | mechanism | growth |
|---|---|---|
| structural | 1 synthetic subgraph per `@connect`; shared entities ⇒ complete cross-subgraph key graph | edges = **O(S²)**, S = N·(K+E) |
| peak heap | `precompute_non_trivial_followup_edges` per-edge Vecs | **O(S²)** (exp 2.02 measured) |
| churn / time | `out_edges`→`sorted_edges` realloc+sort on every call | **≥O(S²)**, total alloc exp 2.6 |
| secondary (linear) | per-synthetic-subgraph HTTP client TLS `RootCertStore` clone; per-subgraph schema extraction | O(S) |

## Fix directions (detailed in Phase 4)

1. **Cache `out_edges`/sorted followup edges** instead of re-sorting on every call — kills
   the 4.77 GiB `sorted_edges` churn. (Biggest, lowest-risk win.)
2. **Avoid materializing `precompute_non_trivial_followup_edges` for O(S²) edges** — compute
   lazily or store compactly; kills 53% of peak.
3. **Reduce the O(S²) key-edge explosion**: don't create a complete cross-subgraph key graph
   among synthetic subgraphs that share a source — the real fix is a source-aware planner /
   sharing synthetic subgraphs per source subgraph rather than per `@connect`.
4. **Share the rustls `RootCertStore` (Arc)** across per-subgraph HTTP clients instead of
   cloning per synthetic subgraph (`http/service.rs:359`) — secondary, linear.
