# Nested output shapes — built (restrictive-provides follow-on item 1)

> **Context.** This implements item 1 of
> `SOURCE_AWARE_RESTRICTIVE_PROVIDES_FOLLOW_ONS.md`, "Nested output shapes — the
> real correctness frontier". That doc planned the work; this one records what
> was actually built, where the plan was wrong, and what it turned out to
> unlock.
>
> **Sibling docs reconciled** in the same change:
>
> - `SOURCE_AWARE_RESTRICTIVE_PROVIDES_FOLLOW_ONS.md` — item 1 marked built with
>   the three corrections to its plan, its verification recipe carries a warning
>   that parity with expansion is only a valid oracle where expansion is correct,
>   and the recommended order is updated.
> - `README.md` — status table no longer lists nested output shapes under "Not
>   built", and a new "Where the two paths diverge, on purpose" section records
>   that flag-*on* now deliberately differs from expansion for this class. The
>   "flag off is byte-identical" claim is unchanged and still true.

## What was wrong

`connector_provided_fields` read only the top level of
`SelectionAnalysis::output_shape()`. A connector selecting

```
id manager { id name } reports { id }
```

reaches `Person` under `manager` with `{id, name}` and under `reports` with
`{id}`. Reading only the top level sees `User`'s fields, concludes the connector
provides all of them, and prunes nothing — so the planner served `Person.name`
from the `users` connector at *both* positions and `reports { name }` came back a
silent `null`.

Reproduction, with the null observed at the wire rather than derived:
`apollo-router/tests/samples/connectors/sibling-position-over-merge/`.

## Three changes, not two

The follow-on doc scoped this as "restructure `connector_provided_fields` and
`restrict_connector_reachability`". That was one site short.

### 1. `connector_provided_tree` (`query_plan/connector_stamp.rs`)

Returns a recursive `ProvidedTree` instead of a flat `HashSet<String>`, with a
three-way `Provided` distinction:

| variant | meaning |
|---|---|
| `Leaf` | provided, terminal — a scalar. Nothing below to restrict. |
| `Opaque` | provided, but the shape could not be read as one object (a union, an intersection, an unknown). Must not restrict below. |
| `Object(ProvidedTree)` | fields known exactly. |

`Leaf` and `Opaque` behave identically today, but conflating them makes it
impossible to tell a scalar field from a shape the derivation gave up on, which
matters when reading a diff to check whether the pass fired.

**Why not memoize on `Shape` directly, as the plan said.** `Shape`'s `Name`
leaves carry position-specific input paths — `$root.*.manager.*.id` — so
structurally identical restrictions reached by different paths hash differently.
Memoizing on `Shape` would silently produce one copy per position with **no
sharing at all**: still correct, but it defeats the memo and inflates node count
on exactly the large selections where it matters. Deriving a path-free tree first
fixes that at the root.

Two things worth knowing about the shape, both verified rather than assumed:

- **List positions are already normalized to the element object.** `reports:
  [Person]` with `reports { id }` yields `reports` as a plain `Object`, not
  `Array { prefix, tail }` — the list lives in the path (`$root.*.reports.*.id`).
  The `Array` arm exists for shapes that do surface one, but it is not the common
  case.
- **Object keys are the GraphQL-facing names.** For `"id alias: original"` the
  key is `alias`, so keys compare directly against
  `field_definition_position.field_name()` at every level. The recursion depends
  on this holding at depth, and it does.

### 2. `restrict_node` (`query_graph/connect_graph.rs`)

Replaces the top-level-only copy logic. Walks the provided tree alongside the
graph; at each level, prunes the field edges that level does not provide and
re-points provided fields whose own type was restricted at that type's restricted
copy rather than the shared full node. Memoized on `(NodeIndex, ProvidedTree)`.

Three things the plan did not anticipate:

- **The candidate predicate had to become recursive too.** `has_prunable_field`
  was computed from the landing node's own out-edges. On the reproduction, `User`
  has exactly `{id, manager, reports}` and the selection provides all three, so
  nothing was prunable at the top level and **the candidate was dropped before
  any copy was made**. The pass was a complete no-op on the very shape it was
  meant to fix. A candidate now survives when something is prunable at *any*
  depth.
- **`has_reentry` is per level**, evaluated on the node being pruned rather than
  once on the landing type. A level with prunable fields but no `KeyResolution`
  re-entry keeps all its fields, preserving today's behaviour instead of newly
  erroring — so follow-on item 2 (no-key semantics) stays an open operator
  decision rather than being settled incidentally. Every such skip now emits a
  `tracing::debug!` naming the type, source and provided set, which is the
  evidence item 2 asked for.
- **A level that prunes nothing may still need a copy**, to carry the re-pointed
  edge to a restricted child. Such a copy needs no re-entry of its own, since it
  hides nothing.

### 3. The third traversal assumption (`query_plan/query_planning_traversal.rs`)

The follow-on doc's warning — "planner optimizations assume *same type ⇒ same
fields reachable*; budget for finding more" — was right, and this is the one it
found.

`selection_set_is_fully_local_from_all_nodes` decides a sub-selection needs no
graph traversal at all and can be attached to the fetch verbatim. Its test:

```rust
if n.has_reachable_cross_subgraph_edges { return Ok(false); }
...
if !selection.can_rebase_on(&parent_ty, schema)? { return Ok(false); }
```

`can_rebase_on` consults the subgraph **schema**, which still has the fields the
copy deliberately pruned. So the copies were built correctly and then bypassed.

Two properties made this hard to see:

- **It is not a depth problem.** The existing top-level test passes only because
  `steelthread.graphql` has a second `graphql` subgraph, so
  `has_reachable_cross_subgraph_edges` is true and the shortcut returns early.
  The reproduction has a single subgraph, so the shortcut fires.
- **Gating on "is this node a copy" is insufficient**, because with one subgraph
  the shortcut fires at `Query` — judging the entire operation local before the
  traversal reaches any copy.

The fix is therefore *reachability*, not a node check:
`QueryGraph::nodes_reaching_connector_boundary_copy`, a reverse BFS from every
copy, computed once by the pass. Empty for every graph the pass did not mutate,
so the expansion path is untouched. The alternative — a single bool disabling the
shortcut for any connector graph — was rejected because it would cost the
optimization on exactly the large connector graphs this branch exists to speed
up.

Unlike the first two relaxations, which eat a path, this one skips path-building
altogether. Worth remembering as a *category* when hunting the next one: not
every assumption shows up as a missing option.

## The oracle inverts

Every prior slice used **parity with the expansion path** as its oracle:
`source_aware_root_field_end_to_end`,
`source_aware_entity_plus_requires_end_to_end`,
`source_aware_multi_connector_merged_fetch_end_to_end`, and
`source_aware_entity_dispatch_end_to_end` all assert byte-for-byte agreement.

**This is the first case where source-aware must disagree with expansion and be
right.** Expansion cannot represent per-position field sets, so parity here would
be the bug. The follow-on doc's recipe ("expansion path as oracle, asserting both
response equality and dispatched-request equality") must not be applied to this
item.

No existing test asserted the buggy output as truth, so nothing had to change:
the four parity tests use schemas where no type appears at two positions with
*different* field sets. A break in any of them remains a genuine regression, not
an expected inversion.

The reproduction sample exercises both paths in one run via
`ReloadConfiguration` — the source-aware flag *is* re-read on config reload —
so the divergence is the artifact rather than something a reader reconstructs
from two directories.

## What this unlocked: `User.friends: [User]`

Recursive output shapes turn out to need **no further work in the planner**.
`plans_recursive_output_shape` (`query_plan/source_aware.rs`, against
`recursive_output.graphql`) is green: with `selection: "id name friends { id }"`,
`{ users { id name } }` is one fetch and `{ users { friends { name } } }` gains an
`_entities` re-entry flattened at `users.@.friends.@`, because `name` is provided
at the root but not under `friends`.

**The recursion terminates by construction.** It walks the finite output *shape*,
not the cyclic type graph: a selection can only nest finitely, so `friends { id }`
bottoms out after one level. Cyclicity in the schema never becomes cyclicity in
the traversal. `MAX_PROVIDED_DEPTH` is insurance against an unforeseen
`ShapeCase::Name` self-reference, not a working bound.

So the blocker on `User.friends: [User]` is **composition validation, not
planning**. `Code::CircularReference` is enforced at `validation/connect.rs:412`
(direct self-reference) and `validation/connect/selection.rs:510` and `:842` (the
nested walk, called from both sides of the pre-v0.4 / v0.4-and-later fork at
`:645` and `:1059`), so it is not version-gated.

**Not established**, and worth stating plainly rather than leaving as an implied
"probably fine":

- nesting depth two or more (`friends { friends { id } }`) is untested;
- a recursive type with no entity resolver hits the no-key decision at every
  level;
- whether anything downstream of planning assumes acyclic connector output
  shapes has not been checked;
- expansion still cannot represent any of this, so the feature would ship gated
  on `experimental_connectors_source_aware` or require the same treatment in
  expansion.

## Verification

- `cargo test -p apollo-federation` — full suite green, including the existing
  top-level test, so the recursion is a strict generalization.
- `cargo test -p apollo-router --test samples --features snapshot sibling` —
  green. **`--features snapshot` is not optional**; without it the sample is
  skipped silently.
- New fed-level tests: `plans_entity_fetch_per_nested_position`,
  `plans_recursive_output_shape`.

Copy count on the reproduction is three (one `User`, two `Person`) for a
two-position selection, with the memo sharing structurally identical
restrictions. The plan's copy-explosion concern looks manageable, but no large
connector graph has been measured.
