# Connector Circular References

## Problem

Connectors create one synthetic subgraph per HTTP endpoint. These endpoints return fixed-depth JSON — they cannot recurse. But the query planner treats each synthetic subgraph as a full GraphQL subgraph capable of resolving recursive types to arbitrary depth.

When a type references itself (directly: `User.friends: [User]`, or indirectly: `User→Book→Author→Book`), the connector's selection must also reference the type, triggering a circular reference validation error. Even if validation were removed, the subgraph schema overpromises: it says `User` has `{id, name, friends}` at every nesting level, when the connector only provides a subset at the nested level.

## Solution: Restricted Copy Nodes

Introduce "restricted copy" nodes in the query graph. A restricted copy represents a type with only a subset of its fields available. When the query planner needs fields not on the restricted copy, it uses a KeyResolution edge (entity lookup) back to the full type node.

### How it works

Given a connector with selection `id name friends { id name }`:

- The connector subgraph has `User { id, name, friends: [User] }`
- `friends` returns `[User]`, but nested Users only have `{id, name}` — NOT `friends`
- The query graph gets:
  - `User(original)`: edges for `id`, `name`, `friends→User(restricted)`
  - `User(restricted)`: edges for `id`, `name` only, plus KeyResolution→`User(original)`
- To get `friends` on nested Users, the planner must re-enter via entity resolution

### Query plan example

```graphql
{ user(id: "1") { name friends { name friends { name } } } }
```

```
Sequence:
  Fetch(connector_query):   { user(id:"1") { name friends { __typename id } } }
  Flatten(user.friends):
    Fetch(connector_entity): { ... on User { name friends { __typename id } } }
  Flatten(user.friends.friends):
    Fetch(connector_entity): { ... on User { name } }
```

Each recursion level is a separate entity resolution fetch, bounded by the query's depth.

## Design

### Encoding: `connectedSelection` on `@join__field`

Restriction metadata is encoded as a compiler artifact in the supergraph using a new argument on `@join__field`:

```graphql
directive @join__field(
  graph: join__Graph,
  requires: join__FieldSet,
  provides: join__FieldSet,
  type: String,
  external: Boolean,
  override: String,
  usedOverridden: Boolean,
  overrideLabel: String,
  connectedSelection: join__FieldSet  # NEW
) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
```

Supergraph encoding:

```graphql
type User
  @join__type(graph: MYAPI_QUERY_USER_0, key: "id")
  @join__type(graph: MYAPI_USER_0, key: "id")
{
  id: ID! @join__field(graph: MYAPI_QUERY_USER_0) @join__field(graph: MYAPI_USER_0)
  name: String @join__field(graph: MYAPI_QUERY_USER_0) @join__field(graph: MYAPI_USER_0)
  friends: [User]
    @join__field(graph: MYAPI_QUERY_USER_0, connectedSelection: "id name")
    @join__field(graph: MYAPI_USER_0, connectedSelection: "id name")
}
```

`connectedSelection: "id name"` means: traversing `friends` in this subgraph yields a User restricted to `{id, name}`. Other fields require entity resolution.

Nested field sets encode multi-level restrictions: `connectedSelection: "id name friends { id }"` creates a chain of restricted copies.

### Phase 1: Lift circular reference validation

**Two validation sites:**

1. `connectors/validation/connect.rs:370-382` — Direct field-level check (`User.friends: [User]` with `@connect`). Remove.
2. `connectors/validation/connect/selection.rs:407-450` — Selection path check (`check_for_circular_reference`). Remove.

The JSONSelection is inherently finite. The Shape tree produced by the selection drives `walk_type_with_shape`, which naturally terminates because it follows the shape, not the type graph.

### Phase 2: Emit `connectedSelection` metadata during expansion

**Connector expansion** (`connectors/expand/visitors/selection.rs`):

Add visited-type tracking to `TypeShapeWalker`:

```rust
struct TypeShapeWalker<'a> {
    // ... existing fields
    walking_types: IndexSet<Name>,  // types currently being walked
    restrictions: Vec<FieldRestriction>,  // recorded restrictions
}
```

In `walk_field_type`, before recursing into a type already being walked:
- Record the restriction: the field name and the Shape's fields at the nested level
- Skip recursion (the type is already added to the schema)

After expansion, emit `connectedSelection` as a directive on the field in the generated subgraph schema. During recomposition into the internal supergraph, this becomes the `connectedSelection` argument on `@join__field`.

**Applies to all connector types:**
- `@connect` on Query/Mutation fields (query connectors)
- `@connect` on type fields with `$this` (single entity resolvers)
- `@connect` on type fields with `$batch` (batch entity resolvers)

### Phase 3: Create restricted copy nodes in the query graph

**New handler** `handle_connected_selection()` in `FederatedQueryGraphBuilder::build()`, after `handle_key()` and before `handle_provides()`.

For each FieldCollection edge whose field has `connectedSelection`:

1. Parse the field set
2. Create a restricted copy node (new node, same type/source, `provide_id = Some(n)`)
3. Do NOT copy FieldCollection edges from original
4. DO copy KeyResolution edges (for entity resolution) — the original's self-key edge (original→original) becomes copy→original on the copy
5. Add FieldCollection edges for only the fields in `connectedSelection` + `__typename`
6. For nested field sets, recurse: create another restricted copy for the next level
7. Redirect the original FieldCollection edge to point to the restricted copy

This is the inverse of `copy_for_provides` (which copies ALL edges then adds more). Restricted copies start empty and add only specified fields.

### Phase 4: Allow self-key re-entry from restricted nodes

**`query_graph/graph_path.rs:1544-1551`** — The re-entry check:

```rust
// Current:
if edge_tail_weight.source == original_source && to_advance.defer_on_tail.is_none() {
    continue;
}

// Changed:
let tail_is_copy = self.graph.node_weight(self.tail)?.provide_id.is_some();
if edge_tail_weight.source == original_source
    && to_advance.defer_on_tail.is_none()
    && !tail_is_copy
{
    continue;
}
```

Safe for existing `@provides` copies: they have ALL original edges, so re-entry is a longer path that cost optimization prunes. Only restricted copies benefit from re-entry because they have fewer edges than the original.

### Phase 5: Satisfiability

No code changes expected. The checker's `can_skip_visit_for_subgraph_paths` memoization handles cycles:

1. Visit `User(original)` with context `[subgraph_A, ...]`
2. Follow `friends` → `User(restricted)` → key → `User(original)`
3. Same state as step 1 → skip (memoized)

Verify experimentally with composition tests.

## Implementation Strategy: De-risking Query Planner Changes

The query planner is the highest-risk area. Strategy: validate the approach using existing federation mechanisms BEFORE touching planner code, then make minimal, additive changes.

### Phase 0: Proof-of-concept with existing federation (zero planner changes)

Write a test using manually crafted subgraphs that proves recursive entity resolution already works:

```graphql
# Subgraph A: has id and name, NOT friends
type Query { user(id: ID!): User }
type User @key(fields: "id") { id: ID! name: String }

# Subgraph B: has id and friends, NOT name
type User @key(fields: "id") { id: ID! friends: [User] }
```

For `{ user { name friends { name friends { name } } } }`, the planner already produces sequential entity resolution fetches alternating A→B→A→B. This is our "north star" — the plan shape we want to reproduce with restricted copies.

The restricted copy mechanism is then an **optimization**: achieving the same plan shape within a single connector subgraph rather than requiring two manually split subgraphs.

### PR structure (3 independent PRs)

**PR1: Validation + expansion** (no planner changes)
- Lift circular reference validation
- Emit `connectedSelection` metadata during expansion
- Tests: validation snapshots, expansion fixtures

**PR2: Query graph construction** (additive only, no existing code modified)
- New `handle_connected_selection()` handler in `build_query_graph.rs`
- Completely parallel to `handle_provides` — same patterns, separate code
- Tests: graph structure assertions, satisfiability, DOT visualization

**PR3: Re-entry check** (one line)
- Single boolean condition added to `graph_path.rs:1548`
- Tests: query plan snapshots, plan equivalence with PoC test, full regression

### Making changes clear to reviewers

1. **Query graph DOT visualization** — Include `to_dot()` output in the PR description showing the graph before and after. Reviewers see exactly what nodes/edges were added.

2. **Plan equivalence test** — Show side-by-side that the connector plan matches the manually-split-subgraph plan from Phase 0. Proves the new mechanism produces the same result as existing federation.

3. **Comparison test** — Same recursive schema, with and without `connectedSelection`. Without: planner tries to resolve everything in one fetch. With: sequential entity resolution.

4. **No shared code with `handle_provides`** — `handle_connected_selection` is a completely separate function. No risk of affecting existing @provides behavior. Reviewers can diff the two functions side-by-side.

5. **Full regression check** — Run `cargo test -p apollo-federation` before and after. Zero existing snapshot changes.

### Why the `graph_path.rs` change is safe

The change adds `&& !tail_is_copy` to the re-entry check. For existing `@provides` copies (which have ALL original edges), re-entry is a strictly longer path to the same edges — the `best_path_by_source` cost comparison (lines 1585-1598) prunes it immediately. Only restricted copies (which have fewer edges than the original) benefit from re-entry. The existing test suite proves no regression.

## Test Plan

### Layer 0: Proof-of-concept (existing federation, no changes)

- Split-subgraph test: A={id,name}, B={id,friends} — proves recursive entity resolution plan shape
- This test validates the target behavior using ZERO new code

### Layer 1: Validation

- Update 4-5 existing snapshots (circular reference errors become empty)
- New fixtures: valid circular selections (direct + indirect cycles)

### Layer 2: Expansion

- New expansion fixture with circular connector schema
- Snapshot: generated subgraph SDL has `connectedSelection` on recursive fields
- Cover: query connector, `$this` entity resolver, `$batch` entity resolver

### Layer 3: Query Graph Structure

- Unit test: build query graph from connector supergraph, assert restricted copy nodes exist
- Assert restricted copy has only specified FieldCollection edges + key edges
- Assert key edge from restricted copy targets the original node
- DOT output snapshot for visual review

### Layer 4: Query Plans (critical)

- Depth-1: `{ user { friends { name } } }` → fetch + entity resolve
- Depth-2: `{ user { friends { friends { name } } } }` → 3-step sequence
- No recursion needed: `{ user { friends { id } } }` → single fetch (id on restricted copy)
- Indirect cycle: `{ user { books { author { books { title } } } } }`
- Self-key re-entry: entity resolver re-enters itself
- **Plan equivalence**: connector plan matches split-subgraph PoC plan

### Layer 5: Composition/Satisfiability

- Circular connector schema composes successfully
- `validate_satisfiability()` returns Ok
- API schema preserves recursive type structure

### Layer 6: Integration (E2E)

- WireMock-based test with mock HTTP connector endpoints
- Verify fetch sequence: query fetch → entity resolution fetches
- Verify response assembly from multiple fetches
- Cover `$this` and `$batch` entity resolvers

## Files Changed

| File | Change |
|------|--------|
| `connectors/validation/connect.rs` | Remove direct circular reference check |
| `connectors/validation/connect/selection.rs` | Remove `check_for_circular_reference` |
| `connectors/expand/visitors/selection.rs` | Add type tracking, record restrictions |
| `connectors/expand/mod.rs` | Emit `connectedSelection` directive on recursive fields |
| `query_graph/build_query_graph.rs` | New `handle_connected_selection()` handler |
| `query_graph/mod.rs` | Possibly extend `QueryGraphNode` if needed |
| `query_graph/graph_path.rs` | Allow self-key re-entry from copy nodes |
| `composition/` join spec definitions | Add `connectedSelection` to `@join__field` |
| Validation snapshots (4-5 files) | Update expected errors |
| New test files (6+ files) | Tests at each layer |

## Risks

| Risk | Mitigation |
|------|-----------|
| Infinite loop in planner | Query depth bounds recursion; satisfiability memoization; defense-in-depth depth guard |
| Existing @provides behavior change | Re-entry from provides copies is pruned by cost optimization; full test suite regression check |
| Performance regression | Minimal graph growth (~1 node + ~3 edges per recursive field); run benchmarks before/after |
| Multi-level nesting complexity | Recursive `add_restricted_edges` mirrors existing `add_provides_edges` pattern |
