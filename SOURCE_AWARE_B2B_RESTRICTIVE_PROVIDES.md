# B-2b via "restrictive provides" — fixing the over-merge (side-effect #3)

> **STATUS: IMPLEMENTED.** `connect_graph::restrict_connector_reachability`,
> wired into `SourceAwareQueryPlanner::new`. The router repro
> `source_aware_entity_resolver_connector_gap` is un-ignored and green; the full
> federation corpus and connectors suite pass unchanged. Two planner-side
> obstacles beyond this design surfaced during implementation (both fixes
> self-gated on the new `QueryGraphNode::connector_boundary_copy` marker, so
> ordinary planning is untouched):
>
> 1. **Precomputed traversal maps.** Post-build mutation invalidates
>    `non_trivial_followup_edges` (traversal hard-errors on missing entries and
>    would never consider new edges) and `non_local_selection_metadata`; the
>    pass recomputes both (the former via a refactored `pub(crate)`
>    `precompute_non_trivial_followup_edges`).
> 2. **Two traversal shortcuts assume "same type ⇒ same fields reachable."**
>    (a) The indirect-search guard skipping edges back to the original source
>    ("direct transition already checked" — false on a pruned copy) now admits
>    same-source re-entry when the search starts on a boundary copy.
>    (b) The detour-elimination optimization (`check_direct_path_from_node`)
>    found a "direct path" ending on the pruned copy itself and discarded the
>    re-entry as a useless detour; a direct path ending on a boundary copy no
>    longer justifies elimination.

> **Goal.** Make the source-aware planner emit a valid `_entities` fetch for a
> connector field the entry connector doesn't provide (e.g. `username` on a
> `Query.users` result), instead of over-merging it into a fetch that returns
> `null`. This reconstructs, *in the query graph*, the per-connector field
> boundary that expansion built as minimal synthetic subgraphs — but by reusing
> existing `@provides` node-copy machinery, so it is spike-scale, not a
> multi-quarter graph rewrite.

## The key realization

`build_query_graph` is **not** strictly one-node-per-type. `@provides` handling
(`handle_provides` → `copy_for_provides` → `update_edge_tail` +
`add_provides_edges`) already **copies a type's node** and re-points a specific
field's in-edge to the copy. Multi-node-per-type is established machinery.

**A connector is a field with an implicit `@provides(<its selection output
shape>)`.** `Query.users @connect(selection: "id name")` behaves like
`Query.users: [User] @provides("id name")` — except the copy must be
**restrictive** (only the connector's output-shape fields), not additive.

## The transformation (per connector field-collection edge, source-aware only)

For each `FieldCollection` edge in the `connectors` subgraph whose field is a
root/entity connector landing on the connector's output type `T`:

1. **Copy** the landing-type node for `T` (mirror `copy_for_provides_internal`:
   `create_new_node` + clone out-edges).
2. **Re-point** this field's in-edge to the copy (mirror `update_edge_tail`).
3. **Prune** the copy's out-edges to the connector's **provided set** —
   `connector_provided_fields()` (SelectionAnalysis output-shape top-level
   fields) ∪ {`__typename`} ∪ the entity `@key` fields. Drop `FieldCollection`
   edges for non-provided fields.
4. **Re-entry:** ensure pruned fields stay reachable via a `KeyResolution` edge
   from the restricted copy to a node for `T` that *does* have them (the full
   node, or the entity-resolver connector's landing). Condition = the entity
   `@key` (`connect_graph.rs::build_source_entering_edge` already derives exactly
   this edge data). The planner then emits an `_entities` fetch for the pruned
   field; the step-3 stamping (entity-resolver match by output type) stamps it
   with the resolving connector's coordinate; dispatch (B-3) routes it. The
   planner keeps ownership of *validity* (it only takes the re-entry when a key +
   resolver actually exist) — so we never do the unsound "translate every missing
   field to `_entities`" rewrite.

## Locus & gating

A **post-build pass**, isolated to the source-aware path so the shared builder
and the expansion path are untouched:

- Call site: `SourceAwareQueryPlanner::new` (or a helper it calls), *after*
  `build_federated_query_graph` returns the `QueryGraph`, before
  `QueryPlanner::from_query_graph`. It has the `connectors` set and schema.
- Implement inside the `query_graph` module (e.g. extend `connect_graph.rs`)
  because `QueryGraph.graph` is a private `DiGraph` with `pub(crate)` mutators;
  the pass needs `add_node`/`add_edge`/out-edge iteration/edge re-point against
  it (replicate the small slice of `copy_for_provides` it needs, or expose a
  `pub(crate)` helper on `QueryGraph`).
- Gating is automatic: only the source-aware raw graph contains unexpanded
  connectors; the expansion path never does. Keep it behind the
  `experimental_connectors_source_aware` flag regardless.

## Why this is bounded

- Reuses proven `@provides` copy + KeyResolution-edge machinery; no new
  transition variant (avoids the ~7-match fan-out the earlier session feared).
- The edge *data* already exists (`connect_graph.rs`, currently `dead_code`).
- The provided-field primitive already exists and is tested
  (`connector_provided_fields`, `connectors@…` step-3 groundwork commit).
- Fully isolated post-build pass → no blast radius on non-connector planning,
  no full-corpus re-validation of the expansion path.

## Scope guards for the first slice

- Root-field entry connectors landing on an entity type with a single `@key`
  (the `{ users { username } }` repro). Defer: interfaces/unions, multi-key,
  nested non-entity objects (those correctly stay unreachable → planner errors,
  which is right), type-level (`TypeBatch`/`TypeSingle`) resolvers.
- Verify flag-off byte-identical + the full corpus's *source-aware* tests; the
  repro `source_aware_entity_resolver_connector_gap` un-ignores when it passes.

## Open risks to watch

- Re-entry target node choice (full node vs resolver landing) and avoiding
  cycles / self-edges that the planner ignores.
- Interaction with the existing `handle_key` self-edges for the connectors
  subgraph `@key`.
- Cost: the re-entry must not make the planner pick a worse plan for queries
  that *were* fine (only prune what the connector genuinely doesn't provide).
