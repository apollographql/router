# Restrictive-provides follow-ons — work plan (planning only, nothing implemented)

> **Context.** The restrictive-provides pass
> (`connect_graph::restrict_connector_reachability`, landed in `781a4af64`)
> fixed the entity-resolver over-merge for its first slice: **top-level fields
> of object-shaped connector output, when a `KeyResolution` re-entry exists.**
> Its conservative guards deliberately skip everything else. This doc plans the
> follow-ons those guards made newly tractable — each now has a working
> template to extend (copy → prune → re-point → recompute traversal maps →
> self-gated traversal relaxations) instead of an open research question.
>
> Ordering below is the recommended build order. Estimates are relative
> t-shirt sizes, not promises.

## Shared foundation (what every item builds on)

- The pass template in `connect_graph.rs`: candidate collection over
  `FieldCollection` edges, node copy with `connector_boundary_copy` marking,
  out-edge cloning with pruning, `update_edge_tail`-style re-pointing, and the
  mandatory recompute of `non_trivial_followup_edges` +
  `non_local_selection_metadata`.
- The traversal lesson: planner optimizations assume **"same type ⇒ same
  fields reachable."** Boundary copies break that invariant, and each new
  slice may trip another such assumption. The working method is: build the
  graph shape, plan-dump a repro, find the guard that eats the path, relax it
  gated on `connector_boundary_copy`. Two are already relaxed (same-source
  re-entry; detour elimination). Budget for finding more.
- Verification recipe (per item): fed-level plan-shape test in
  `source_aware.rs` → router end-to-end repro in
  `plugins/connectors/tests/mod.rs` (expansion path as oracle, asserting both
  response equality and dispatched-request equality) → full fed corpus +
  connectors suite for flag-off/regression safety.

  > **The oracle does not always hold — it did not for item 1.** Parity with
  > expansion is only a valid oracle where expansion is *correct*. Item 1's
  > whole subject is a shape expansion cannot represent, so there parity would
  > have been the bug and the test had to assert **divergence** instead. Before
  > applying this recipe to an item, ask whether expansion gets that case right;
  > if it does not, the recipe is inverted for that item. See
  > `SOURCE_AWARE_NESTED_POSITIONS.md`, "The oracle inverts".

## 1. Nested output shapes (M–L) — the real correctness frontier

> **BUILT.** See `SOURCE_AWARE_NESTED_POSITIONS.md` for what was actually done,
> where this plan was wrong, and what it turned out to unlock. Three corrections
> to the plan below, kept here because they are the kind of thing the next item
> will hit too:
>
> - **A third site.** The candidate predicate had to become recursive as well.
>   `has_prunable_field` read only the landing node's own out-edges, so on a
>   schema where every top-level field *is* provided, the candidate was dropped
>   before any copy was made — the pass was a **no-op on the very shape it was
>   meant to fix**.
> - **`shape_node` is not a viable memo key.** `Shape`'s `Name` leaves carry
>   position-specific input paths (`$root.*.manager.*.id`), so structurally
>   identical restrictions hash differently and never share a copy. Derive a
>   path-free tree first.
> - **The traversal assumption was found, and it is a new category.** Not a
>   guard that eats a path, but `selection_set_is_fully_local_from_all_nodes`
>   skipping path-building altogether. Fixed by *reachability* of a boundary
>   copy, not a check on the node itself, since with a single-subgraph connector
>   supergraph it fires at `Query`.
>
> It also turned out to unlock recursive output shapes (`User.friends: [User]`)
> with no further planner work; the remaining blocker there is
> `Code::CircularReference` validation.

**Gap.** `connector_provided_fields` reads only the *top level* of
`SelectionAnalysis::output_shape()`. A connector whose selection returns
`address { city }` still lands on a shared `Address` node exposing `zip`; the
planner happily merges `zip` into the fetch → silent null. Same family as the
original bug, one level down.

**Plan shape.**
- Walk the output shape **recursively** alongside the graph: for each
  object-typed field the connector provides, the copy's field edge must point
  at a **recursively restricted copy** of the field's landing type, pruned to
  the shape's nested provided set — not at the shared full node.
- Memoize copies per `(connector, type_position, shape_node)`; connector
  selections can hit the same type at several paths with different sub-shapes,
  and recursive types make memoization mandatory (cycle guard).
- Re-entry per level: nested **entity** types (keyed) get the same cloned
  self-key re-entry as the top level — pruned fields become `_entities`
  fetches. Nested **non-entity** types have no key: pruned fields become
  planner errors, which is the *correct* outcome (see item 2 for the
  null-vs-error decision — resolve item 2 first so the semantics are chosen
  deliberately, not incidentally).
- Expect new traversal assumptions to surface here (e.g. downcast edges on
  nested abstract types cloned onto copies). Same probe → guard → self-gate
  method.

**Repro to write first:** steel-thread-style schema where `Query.users`
returns `address { city }` and the query selects `address { zip }`; today
expansion forces a resolver (or composition rejects), source-aware nulls.

**Risks.** Copy explosion (nodes × connectors × shape depth) — bounded in
practice by connector selection sizes, but add a counter/log line. Interaction
with `handle_provides` copies of nested types (copies of copies) needs a test.

## 2. No-key semantics: silent null → loud failure (S, decision-heavy)

**Gap.** When a connector under-provides but the landing type has **no**
`KeyResolution` re-entry, the pass skips the transform, preserving today's
over-merge → `username`-style silent null. Pruning without re-entry would turn
that into a **planning error** — honest, but a behavior change.

**Plan shape.**
- Decide the semantics first (operator call): (a) plan-time error — field is
  selectable in the API schema but unresolvable; arguably the supergraph
  should not have composed; (b) keep null but emit a **diagnostic** (warning
  log/metric at planner-build time listing unreachable
  `(connector, field)` pairs); (c) both, behind a strictness knob.
- Mechanically trivial once decided: drop the `has_reentry` guard (for (a)),
  or keep it and add a diagnostics sweep that reports every prunable field
  with no re-entry (for (b)).
- Recommendation: **(b) now** — cheap, zero behavior change, and produces the
  evidence for whether (a) should ever ship. Composition-time validation is
  the eventual right home, but that's the expansion-parity question, not this
  spike.

## 3. Type-level entity resolvers — `TypeBatch` / `TypeSingle` (M)

**Gap.** The graph pass emits `_entities` fetches, but the **stamping** match
(`stamp_fetch`'s entity-resolver pass) is scoped to
`EntityResolver::Explicit`. A `@connect` on the type itself (`TypeBatch` via
`$batch`, `TypeSingle` via `$this`) resolving the pruned fields would leave
the fetch unstamped → mis-dispatch under B-3.

**Plan shape.**
- Stamping half: extend the output-type match to
  `Some(TypeBatch) | Some(TypeSingle)`; for type-level connectors the
  "output type" *is* the annotated type, so the match key is simpler than
  Explicit's `base_type_name` derivation — verify what `id.directive`
  exposes for type-level connectors (`simple_name` has no field part).
- Graph half: confirm `derive_condition`/`resolvable_key` produce the key
  condition for type-level resolvers (the connect-graph tests glob over the
  expand fixtures — check whether any fixture exercises TypeBatch; if not,
  add one).
- Batch semantics ($batch = one call for N representations) are a
  **dispatch/cost** concern, not a planning one — the plan shape is identical;
  note it as input to the step-4 cost model (a batch resolver makes the
  re-entry genuinely cheap).

## 4. Multi-connector variants on one field (S–M, partly cost-model era)

**Gap.** Several connectors on the same field (`Query.users[0]`, `[1]`) are
handled by pruning to the **union** of their provided sets — correct-ish but
imprecise: a field only variant `[1]` provides stays reachable even when
dispatch (stamping picks a single coordinate) would route to `[0]`.

**Plan shape.**
- Near-term (correctness): make stamping and the union consistent — if
  stamping dispatches merged fetches to variant `[0]`, the prune set should be
  variant `[0]`'s provided set, not the union. Small, but needs a repro
  fixture with divergent variant selections to pin current behavior first.
- Real fix (cost-model era): one boundary copy **per variant**, letting the
  planner choose a variant by what it provides and (later) by cost. This is
  the first place per-connector identity would appear as *alternative graph
  paths* — a genuine B-2b stepping stone, and exactly where a
  connectors-aware cost model gets its choices from. Defer until step 4
  exists; the union keeps us safe meanwhile.

## 5. Interfaces / unions as connector output (M, defer until a fixture demands it)

**Gap.** Scope guard: abstract-typed landing nodes are skipped (candidate
filter requires `SchemaType` object semantics implicitly via provided-fields
being object-shaped). A connector returning an interface would carry downcast
edges the prune logic doesn't understand.

**Plan shape.** Prune per **runtime type**: the copy keeps downcast edges,
but each downcast target must itself be a restricted copy pruned to the
shape's per-fragment provided fields (`... on A { x }`). Structurally the
same recursion as item 1 — build item 1 first; this becomes its
abstract-type case rather than a separate mechanism. No known fixture
exercises this today; write the fixture before deciding it matters.

## Explicit non-goals (adjacent, but NOT unlocked by this pass)

- **2A entity-field / variable-bearing merges.** A merged `_entities` fetch
  spanning two *field* connectors (`User.d` + `User.e`) is a fetch-level
  sibling merge on the full node; restrictive copies apply to *landing types
  of entry edges*, and a scalar-returning field connector has no landing type
  to restrict. That split stays plan-time (extend
  `split_root_field_fetch` to representations), unchanged in
  priority.
- **Root-fetch decomposition via copies.** Copying `Query`-typed nodes per
  connector cannot split fetches: fetch grouping is by *source*, not node.
  Per-source identity (full B-2b) remains the only graph-level answer there;
  2A's plan-time split remains the working mechanism.

## Recommended order and why

1. ~~**Item 2 (diagnostic flavor)**~~ — the diagnostic half landed with item 1:
   every level left over-merged for want of a re-entry now emits a
   `tracing::debug!` naming the type, source and provided set. The **decision**
   between plan-time error, diagnostic-only, and a strictness knob is still
   open, and is still the operator's.
2. ~~**Item 1**~~ — **done**, ahead of item 2 rather than after it. That
   ordering held up: the per-level conservatism keeps today's behaviour wherever
   no re-entry exists, so item 2's semantics stay genuinely open rather than
   being settled incidentally, which was the reason for the original ordering.
3. **Item 3** — small, closes the stamping scope note, unblocks type-level
   connector users. Now the next one.
4. **Items 4/5** — behind a fixture or the cost model, respectively. Item 5
   (interfaces/unions) is closer than it was: it was specified as "structurally
   the same recursion as item 1", and that recursion now exists, so it becomes
   the abstract-type case of `restrict_node` rather than new machinery.

None of this blocks the step-4 cost-model demo (the divergence fixture uses
field connectors and top-level shapes only); the demo can proceed in parallel
or first, per operator priority.
