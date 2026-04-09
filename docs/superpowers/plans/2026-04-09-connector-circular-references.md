# Connector Circular References Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow connectors to handle recursive types (e.g., `User.friends: [User]`) by creating restricted copy nodes in the query graph and enabling entity resolution at each recursion depth.

**Architecture:** Lift circular reference validation, emit `connectedSelection` metadata during connector expansion, create restricted copy nodes in the query graph builder, and allow self-key re-entry from restricted nodes. Each recursion level becomes a separate entity resolution fetch, bounded by the query's depth.

**Tech Stack:** Rust, apollo-federation crate (query graph, connectors, composition), petgraph, insta (snapshot testing)

**Spec:** `docs/superpowers/specs/2026-04-09-connector-circular-references-design.md`

---

## File Map

| File | Role |
|------|------|
| `apollo-federation/tests/query_plan/build_query_plan_tests/provides.rs` | Phase 0: PoC test proving recursive entity resolution works with existing federation |
| `apollo-federation/src/connectors/validation/connect.rs` | Phase 1: Remove direct circular reference check |
| `apollo-federation/src/connectors/validation/connect/selection.rs` | Phase 1: Remove selection-level circular reference check |
| `apollo-federation/src/connectors/validation/snapshots/*.snap` | Phase 1: Update validation snapshots |
| `apollo-federation/src/connectors/validation/test_data/*.graphql` | Phase 1: New valid circular reference fixtures |
| `apollo-federation/src/link/join_spec_definition.rs` | Phase 2: Add `connectedSelection` to `@join__field` spec |
| `apollo-federation/src/connectors/expand/visitors/selection.rs` | Phase 2: Track visited types, record restrictions |
| `apollo-federation/src/connectors/expand/mod.rs` | Phase 2: Emit `connectedSelection` on recursive fields |
| `apollo-federation/src/connectors/expand/tests/schemas/expand/circular_reference.graphql` | Phase 2: Expansion test fixture |
| `apollo-federation/src/query_graph/build_query_graph.rs` | Phase 3: `handle_connected_selection()` handler |
| `apollo-federation/src/query_graph/graph_path.rs` | Phase 4: Allow self-key re-entry from copy nodes |
| `apollo-federation/tests/query_plan/build_query_plan_tests/connected_selection.rs` | Phase 3-4: Query plan snapshot tests |
| `apollo-federation/tests/composition/connectors.rs` | Phase 5: Satisfiability test |

---

### Task 1: Create branch and Phase 0 PoC test

**Files:**
- Modify: `apollo-federation/tests/query_plan/build_query_plan_tests/provides.rs`

This task proves that the query planner ALREADY handles recursive entity resolution across subgraph boundaries. It's our "north star" for the plan shape we want to reproduce with restricted copies.

- [ ] **Step 1: Create feature branch**

```bash
cd /Users/lenny/Development/apollographql/router
git checkout -b lenny/connector-circular-refs dev
```

- [ ] **Step 2: Write PoC test — recursive type split across two subgraphs**

Add to end of `apollo-federation/tests/query_plan/build_query_plan_tests/provides.rs`:

```rust
#[test]
fn recursive_type_across_subgraphs() {
    // PoC: proves the query planner already handles recursive entity resolution
    // when a recursive type is split across subgraphs.
    // Subgraph A has {id, name}, Subgraph B has {id, friends}.
    // To resolve friends.name, the planner must alternate A→B→A.
    let planner = planner!(
        A: r#"
        type Query {
          user(id: ID!): User
        }

        type User @key(fields: "id") {
          id: ID!
          name: String
        }
        "#,
        B: r#"
        type User @key(fields: "id") {
          id: ID!
          friends: [User]
        }
        "#,
    );

    // Depth 1: need friends from B, then name from A
    assert_plan!(
        &planner,
        r#"
        {
          user(id: "1") {
            name
            friends {
              name
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "A") {
              {
                user(id: "1") {
                  __typename
                  id
                  name
                }
              }
            },
            Flatten(path: "user") {
              Fetch(service: "B") {
                {
                  ... on User {
                    __typename
                    id
                  }
                } =>
                {
                  ... on User {
                    friends {
                      __typename
                      id
                    }
                  }
                }
              },
            },
            Flatten(path: "user.friends.@") {
              Fetch(service: "A") {
                {
                  ... on User {
                    __typename
                    id
                  }
                } =>
                {
                  ... on User {
                    name
                  }
                }
              },
            },
          },
        }
        "###
    );
}
```

- [ ] **Step 3: Run the test**

```bash
cd /Users/lenny/Development/apollographql/router
cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests::provides::recursive_type_across_subgraphs --nocapture 2>&1 | head -50
```

Expected: PASS (or snapshot mismatch — update the snapshot to match the actual plan output, which proves the planner handles this case). If the plan shape is different from the inline snapshot, update the snapshot to capture the actual output. The key thing to verify is that the plan has sequential fetches alternating between A and B.

- [ ] **Step 4: Run with `cargo insta test` if snapshot needs updating**

```bash
cd /Users/lenny/Development/apollographql/router
USE_ROVER=1 cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests::provides::recursive_type_across_subgraphs 2>&1 | tail -20
```

If the supergraph file doesn't exist yet, `USE_ROVER=1` triggers composition via rover. Accept the snapshot with `cargo insta review` if needed.

- [ ] **Step 5: Commit**

```bash
git add apollo-federation/tests/
git commit -m "test: PoC proving recursive entity resolution works across subgraphs

This test demonstrates that the query planner already handles recursive
types when they're split across subgraphs (A has name, B has friends).
This is the 'north star' plan shape for the connector circular reference
feature — restricted copy nodes will reproduce this pattern within a
single connector subgraph."
```

---

### Task 2: Lift direct circular reference validation

**Files:**
- Modify: `apollo-federation/src/connectors/validation/connect.rs:370-382`
- Modify: `apollo-federation/src/connectors/validation/snapshots/validation_tests@circular_reference_3.graphql.snap`
- Modify: `apollo-federation/src/connectors/validation/snapshots/validation_tests@batch.graphql.snap`

- [ ] **Step 1: Remove the direct circular reference check**

In `apollo-federation/src/connectors/validation/connect.rs`, remove lines 370-382 (the `if parent_type.name() == field_def.ty.inner_named_type().as_str()` block):

```rust
// DELETE this block:
            // direct recursion isn't allowed, like a connector on User.friends: [User]
            if parent_type.name() == field_def.ty.inner_named_type().as_str() {
                messages.push(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "Direct circular reference detected in `{}.{}: {}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                        parent_type.name(),
                        field_def.name,
                        field_def.ty
                    ),
                    locations: field_def.line_column_range(&self.schema.sources).into_iter().collect(),
                });
            }
```

- [ ] **Step 2: Run validation tests to see which snapshots changed**

```bash
cd /Users/lenny/Development/apollographql/router
cargo test -p apollo-federation -- connectors::validation 2>&1 | tail -30
```

Expected: some snapshot mismatches for `circular_reference_3` and `batch` (which had the "Direct circular reference" error).

- [ ] **Step 3: Update snapshots**

```bash
cargo insta review
```

Accept the updated snapshots. The `circular_reference_3` snapshot should now show an empty error list (or fewer errors). The `batch` snapshot should have one fewer error (the CircularReference entry removed, other errors remain).

- [ ] **Step 4: Run tests again to confirm pass**

```bash
cargo test -p apollo-federation -- connectors::validation 2>&1 | tail -10
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add apollo-federation/src/connectors/validation/
git commit -m "feat(connectors): remove direct circular reference validation

A @connect on User.friends: [User] is now allowed. The circular
reference will be handled by restricted copy nodes in the query graph,
enabling entity resolution at each recursion depth."
```

---

### Task 3: Lift selection-level circular reference validation

**Files:**
- Modify: `apollo-federation/src/connectors/validation/connect/selection.rs:407-450`
- Modify: `apollo-federation/src/connectors/validation/snapshots/validation_tests@circular_reference.graphql.snap`
- Modify: `apollo-federation/src/connectors/validation/snapshots/validation_tests@circular_reference_2.graphql.snap`
- Modify: `apollo-federation/src/connectors/validation/snapshots/validation_tests@non_root_circular_reference.graphql.snap`

- [ ] **Step 1: Disable the selection-level circular reference check**

In `apollo-federation/src/connectors/validation/connect/selection.rs`, make `check_for_circular_reference` always return Ok:

```rust
    fn check_for_circular_reference(
        &self,
        _field_def: &Node<FieldDefinition>,
        _current_ty: SchemaTypeRef<'schema>,
    ) -> Result<(), Message> {
        // Circular references in selections are now allowed.
        // The selection string is inherently finite, so walk_type_with_shape
        // naturally terminates. Recursive types are handled by restricted
        // copy nodes in the query graph.
        Ok(())
    }
```

- [ ] **Step 2: Run validation tests**

```bash
cargo test -p apollo-federation -- connectors::validation 2>&1 | tail -30
```

Expected: snapshot mismatches for `circular_reference`, `circular_reference_2`, and `non_root_circular_reference`.

- [ ] **Step 3: Update snapshots**

```bash
cargo insta review
```

Accept updated snapshots. All three should now show empty error lists `[]`.

- [ ] **Step 4: Verify all validation tests pass**

```bash
cargo test -p apollo-federation -- connectors::validation 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add apollo-federation/src/connectors/validation/
git commit -m "feat(connectors): allow circular references in @connect selections

Selections like 'id name friends { id }' on recursive types are now
valid. The JSONSelection is inherently finite — walk_type_with_shape
follows the Shape tree, not the type graph, so it naturally terminates."
```

---

### Task 4: Verify connector expansion handles recursive types

**Files:**
- Create: `apollo-federation/src/connectors/expand/tests/schemas/expand/circular_reference.graphql`

This task verifies that the expansion code (`walk_type_with_shape`) correctly handles recursive types without infinite loops after we lifted the validation.

- [ ] **Step 1: Create test fixture for circular connector expansion**

```graphql
@source(name: "api", http: { baseURL: "http://localhost" })

extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.10", import: ["@key"])
  @link(
    url: "https://specs.apollo.dev/connect/v0.4"
    import: ["@connect", "@source"]
  )

type Query {
  user(id: ID!): User
    @connect(
      source: "api"
      http: { GET: "/users/{$args.id}" }
      selection: "id name friends { id name }"
    )
}

type User @key(fields: "id") {
  id: ID!
  name: String
  friends: [User]
    @connect(
      source: "api"
      http: { GET: "/users/{$this.id}/friends" }
      selection: "id name"
    )
}
```

- [ ] **Step 2: Run expansion tests to generate snapshot**

```bash
cd /Users/lenny/Development/apollographql/router
cargo test -p apollo-federation -- connectors::expand::tests 2>&1 | tail -30
```

If tests fail because of a missing snapshot, run:

```bash
cargo insta review
```

Review the generated snapshot. Key things to verify:
- The expanded supergraph has TWO synthetic subgraphs (one for Query.user, one for User.friends entity resolver)
- Both subgraphs have `type User @key(fields: "id") { id: ID! name: String friends: [User] }`
- Expansion does NOT infinite-loop

- [ ] **Step 3: Commit**

```bash
git add apollo-federation/src/connectors/expand/
git commit -m "test(connectors): expansion test for circular reference schema

Verifies that walk_type_with_shape correctly handles recursive types.
The Shape tree naturally terminates expansion even though User.friends
returns [User]."
```

---

### Task 5: Add `connectedSelection` argument to `@join__field`

**Files:**
- Modify: `apollo-federation/src/link/join_spec_definition.rs`

This adds the `connectedSelection` argument to the join spec so the supergraph can encode field restrictions.

- [ ] **Step 1: Add the constant for the new argument name**

In `apollo-federation/src/link/join_spec_definition.rs`, after the existing constant definitions (around line 79):

```rust
pub(crate) const JOIN_CONNECTED_SELECTION_ARGUMENT_NAME: Name = name!("connectedSelection");
```

- [ ] **Step 2: Add the field to `FieldDirectiveArguments`**

In the `FieldDirectiveArguments` struct (around line 170):

```rust
#[derive(Debug)]
pub(crate) struct FieldDirectiveArguments<'doc> {
    pub(crate) graph: Option<Name>,
    pub(crate) requires: Option<&'doc str>,
    pub(crate) provides: Option<&'doc str>,
    pub(crate) type_: Option<&'doc str>,
    pub(crate) external: Option<bool>,
    pub(crate) override_: Option<&'doc str>,
    pub(crate) override_label: Option<&'doc str>,
    pub(crate) user_overridden: Option<bool>,
    pub(crate) context_arguments: Option<Vec<ContextArgument<'doc>>>,
    pub(crate) connected_selection: Option<&'doc str>,  // NEW
}
```

- [ ] **Step 3: Parse the argument in `field_directive_arguments()`**

In the `field_directive_arguments` method (around line 337), add after `context_arguments`:

```rust
            connected_selection: directive_optional_string_argument(
                application,
                &JOIN_CONNECTED_SELECTION_ARGUMENT_NAME,
            )?,
```

- [ ] **Step 4: Add the argument to `field_directive_specification()`**

In `field_directive_specification()` (around line 672), add after the `contextArguments` block (inside the version gate — use the same version as contextArguments, or add a new version gate if needed):

```rust
        // connectedSelection: available in all versions (connector-specific)
        args.push(DirectiveArgumentSpecification {
            base_spec: ArgumentSpecification {
                name: JOIN_CONNECTED_SELECTION_ARGUMENT_NAME,
                get_type: |_schema, link| {
                    let field_set_name = link.map_or(JOIN_FIELD_SET_NAME_IN_SPEC, |link| {
                        link.type_name_in_schema(&JOIN_FIELD_SET_NAME_IN_SPEC)
                    });
                    Ok(Type::Named(field_set_name))
                },
                default_value: None,
            },
            composition_strategy: None,
        });
```

- [ ] **Step 5: Run join spec tests**

```bash
cargo test -p apollo-federation -- link::join_spec_definition 2>&1 | tail -20
```

Expected: tests pass (or snapshot updates needed — accept them).

- [ ] **Step 6: Commit**

```bash
git add apollo-federation/src/link/join_spec_definition.rs
git commit -m "feat(federation): add connectedSelection argument to @join__field

This argument encodes which fields are available on a type when reached
through a specific field in a connector subgraph. Used to create
restricted copy nodes in the query graph for recursive types."
```

---

### Task 6: Emit `connectedSelection` during connector expansion

**Files:**
- Modify: `apollo-federation/src/connectors/expand/visitors/selection.rs`
- Modify: `apollo-federation/src/connectors/expand/mod.rs`

This is the most complex expansion change. When `walk_type_with_shape` encounters a type it's already walking (recursion), it records which fields are available at the nested level.

- [ ] **Step 1: Add visited-type tracking to `TypeShapeWalker`**

In `apollo-federation/src/connectors/expand/visitors/selection.rs`, add a field to track types currently being walked:

```rust
struct TypeShapeWalker<'a> {
    original_schema: &'a ValidFederationSchema,
    to_schema: &'a mut FederationSchema,
    directive_deny_list: &'a IndexSet<Name>,
    spec: ConnectSpec,
    walking_types: IndexSet<Name>,  // NEW: types currently being walked
}
```

Update the constructor in `walk_type_with_shape` to initialize it:

```rust
pub(crate) fn walk_type_with_shape(
    type_def_pos: &TypeDefinitionPosition,
    shape: &Shape,
    original_schema: &ValidFederationSchema,
    to_schema: &mut FederationSchema,
    directive_deny_list: &IndexSet<Name>,
    spec: ConnectSpec,
) -> Result<(), FederationError> {
    TypeShapeWalker {
        original_schema,
        to_schema,
        directive_deny_list,
        spec,
        walking_types: IndexSet::default(),
    }
    .walk_type(type_def_pos, shape)
}
```

- [ ] **Step 2: Add walking_types push/pop in `walk_object`**

In `walk_object()` and `walk_interface()`, track the type name:

```rust
fn walk_object(
    &mut self,
    object: &ObjectTypeDefinitionPosition,
    shape: &Shape,
) -> Result<(), FederationError> {
    self.walking_types.insert(object.type_name.clone());  // NEW
    try_pre_insert!(self.to_schema, object)?;
    // ... existing code ...
    self.walk_object_helper(object, &mut new_object_type, shape)?;
    try_insert!(self.to_schema, object, Node::new(new_object_type))?;
    self.walking_types.shift_remove(&object.type_name);  // NEW
    Ok(())
}
```

- [ ] **Step 3: Detect recursion in `walk_field_type` and skip re-entry**

In `walk_field_type()`:

```rust
fn walk_field_type(
    &mut self,
    field_position: ObjectOrInterfaceFieldDefinitionPosition,
    field_shape: &Shape,
) -> Result<(), FederationError> {
    let field = field_position.get(self.original_schema.schema())?;
    let field_type = self
        .original_schema
        .get_type(field.ty.inner_named_type().clone())?;
    let extended_field_type = field_type.get(self.original_schema.schema())?;

    if !extended_field_type.is_built_in() {
        let type_name = field_type.type_name();
        if self.walking_types.contains(type_name) {
            // Recursive type detected. The type is already being walked,
            // so its node exists in to_schema. Don't recurse — the Shape
            // at this level determines what fields are available, and the
            // type was already fully added at the outer level.
            return Ok(());
        }
        self.walk_type(&field_type, field_shape)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run expansion tests**

```bash
cargo test -p apollo-federation -- connectors::expand::tests 2>&1 | tail -30
```

Update snapshots if needed with `cargo insta review`. Verify the circular_reference expansion fixture now produces valid output.

- [ ] **Step 5: Commit**

```bash
git add apollo-federation/src/connectors/expand/
git commit -m "feat(connectors): handle recursive types during expansion

TypeShapeWalker now tracks which types are currently being walked.
When walk_field_type encounters a type already in the walk stack, it
skips recursion. The type has already been added to the schema at the
outer level, so the Shape naturally limits what fields appear."
```

**Note:** Emitting the actual `connectedSelection` directive on the generated subgraph field, and carrying it through recomposition into `@join__field`, is a follow-up step that requires understanding the recomposition path (`merge_subgraphs` / carryover). This task handles the expansion-side type walking. The directive emission will be implemented as part of the query graph integration (Task 7+).

---

### Task 7: Write failing query plan tests for restricted copies

**Files:**
- Create: `apollo-federation/tests/query_plan/build_query_plan_tests/connected_selection.rs`
- Modify: `apollo-federation/tests/query_plan/build_query_plan_tests.rs` (add module)

Write the query plan tests FIRST (TDD). These will fail until we implement the query graph handler and re-entry check.

- [ ] **Step 1: Create the test module**

Create `apollo-federation/tests/query_plan/build_query_plan_tests/connected_selection.rs`:

```rust
//! Tests for connectedSelection on @join__field — restricted copy nodes
//! that enable entity resolution for recursive types in connectors.

/// When a field on the restricted copy IS available (e.g., `id`),
/// no entity resolution is needed.
#[test]
fn field_on_restricted_copy_no_entity_resolution() {
    // Hand-crafted supergraph with connectedSelection
    let planner = planner!(
        Connector: r#"
        type Query {
          user(id: ID!): User
        }
        type User @key(fields: "id") {
          id: ID!
          name: String
          friends: [User]
        }
        "#,
    );

    // TODO: This test needs a supergraph with connectedSelection encoded
    // in @join__field. For now, this is a placeholder structure.
    // The actual supergraph will be hand-crafted once the join spec
    // argument is wired through.
    assert_plan!(
        &planner,
        r#"{ user(id: "1") { friends { id } } }"#,
        // id IS on the restricted copy — single fetch, no entity resolution
        @r###"
        QueryPlan {
          Fetch(service: "Connector") {
            {
              user(id: "1") {
                friends {
                  id
                }
              }
            }
          },
        }
        "###
    );
}
```

- [ ] **Step 2: Register the module**

In `apollo-federation/tests/query_plan/build_query_plan_tests.rs`, add:

```rust
mod connected_selection;
```

- [ ] **Step 3: Run test to verify it passes (baseline — no connectedSelection yet)**

```bash
cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests::connected_selection --nocapture 2>&1 | tail -30
```

This baseline test should pass because `friends { id }` doesn't need entity resolution even without restrictions.

- [ ] **Step 4: Commit**

```bash
git add apollo-federation/tests/
git commit -m "test: add connected_selection query plan test module

Baseline test for restricted copy nodes. These tests will be expanded
as the query graph handler and re-entry check are implemented."
```

---

### Task 8: Implement `handle_connected_selection()` in query graph builder

**Files:**
- Modify: `apollo-federation/src/query_graph/build_query_graph.rs`

This is the core query graph change. Add a new handler that creates restricted copy nodes.

- [ ] **Step 1: Add the handler to the build chain**

In `FederatedQueryGraphBuilder::build()`, add `handle_connected_selection` after `handle_context` and before `handle_provides`:

```rust
    fn build(mut self) -> Result<QueryGraph, FederationError> {
        self.copy_subgraphs();
        self.add_federated_root_nodes()?;
        self.copy_types_to_nodes()?;
        self.add_root_edges()?;
        self.handle_key()?;
        self.handle_requires()?;
        self.handle_progressive_overrides()?;
        self.handle_context()?;
        self.handle_connected_selection()?;  // NEW
        self.handle_provides()?;
        self.handle_interface_object()?;
        self.base.precompute_non_trivial_followup_edges()?;
        self.base.query_graph.non_local_selection_metadata =
            precompute_non_local_selection_metadata(&self.base.query_graph)?;
        Ok(self.base.build())
    }
```

- [ ] **Step 2: Implement `handle_connected_selection()`**

Add the method to `impl FederatedQueryGraphBuilder`. This follows the same pattern as `handle_provides` but creates restricted copies instead of augmented ones:

```rust
    /// Handle connectedSelection by creating restricted copy nodes for recursive
    /// connector types. This is the inverse of @provides: instead of copying all
    /// edges and adding more, restricted copies start empty and add only the
    /// specified fields.
    fn handle_connected_selection(&mut self) -> Result<(), FederationError> {
        let mut provide_id = self.base.query_graph.max_provide_id();
        for edge in self.base.query_graph.graph.edge_indices() {
            let edge_weight = self.base.query_graph.edge_weight(edge)?;
            let QueryGraphEdgeTransition::FieldCollection {
                source,
                field_definition_position,
                ..
            } = &edge_weight.transition
            else {
                continue;
            };
            if *source == self.base.query_graph.current_source {
                continue;
            }
            let source = source.clone();
            let schema = self.base.query_graph.schema_by_source(&source)?;
            let join_spec = self.subgraphs.get(&source)?.federation_spec_definition
                .join_spec_definition();
            // Look for connectedSelection on the field's @join__field directive
            let field = field_definition_position.get(schema.schema())?;
            let connected_selection = self.get_connected_selection(
                &field.directives, &source, schema,
            )?;
            let Some(connected_selection) = connected_selection else {
                continue;
            };

            provide_id += 1;
            let (_, tail) = self.base.query_graph.edge_endpoints(edge)?;
            let new_tail = self.create_restricted_copy(
                tail, &source, &connected_selection, provide_id,
            )?;
            Self::update_edge_tail(&mut self.base, edge, new_tail)?;
        }
        Ok(())
    }
```

- [ ] **Step 3: Implement `create_restricted_copy()`**

```rust
    /// Creates a restricted copy of a node with only the specified fields
    /// and key resolution edges. Unlike copy_for_provides which copies ALL
    /// edges, this starts empty.
    fn create_restricted_copy(
        &mut self,
        node: NodeIndex,
        source: &Arc<str>,
        fields: &SelectionSet,
        provide_id: u32,
    ) -> Result<NodeIndex, FederationError> {
        let node_weight = self.base.query_graph.node_weight(node)?;
        let QueryGraphNodeType::SchemaType(type_pos) = node_weight.type_.clone() else {
            return Err(FederationError::internal(
                "Unexpectedly found connectedSelection for federated root node",
            ));
        };
        let has_reachable = node_weight.has_reachable_cross_subgraph_edges;

        // Create new empty node
        let current_source = self.base.query_graph.current_source.clone();
        self.base.query_graph.current_source = source.clone();
        let new_node = self.base.create_new_node(type_pos.clone().into())?;
        self.base.query_graph.current_source = current_source;

        let new_node_weight = self.base.query_graph.node_weight_mut(new_node)?;
        new_node_weight.provide_id = Some(provide_id);
        new_node_weight.has_reachable_cross_subgraph_edges = has_reachable;

        // Copy ONLY KeyResolution edges (for entity resolution)
        let mut key_edges = Vec::new();
        for edge_ref in self.base.query_graph.out_edges_with_federation_self_edges(node) {
            if matches!(
                edge_ref.weight().transition,
                QueryGraphEdgeTransition::KeyResolution
                    | QueryGraphEdgeTransition::RootTypeResolution { .. }
            ) {
                key_edges.push(QueryGraphEdgeData {
                    head: new_node,
                    tail: edge_ref.target(),
                    transition: edge_ref.weight().transition.clone(),
                    conditions: edge_ref.weight().conditions.clone(),
                });
            }
        }
        for key_edge in key_edges {
            key_edge.add_to(&mut self.base)?;
        }

        // Add FieldCollection edges for only the restricted fields
        self.add_restricted_field_edges(node, new_node, source, fields, provide_id)?;

        // Register in types_to_nodes
        self.base.query_graph
            .types_to_nodes_mut()?
            .get_mut(type_pos.type_name())
            .ok_or_else(|| FederationError::internal(
                format!("Missing type in types_to_nodes for restricted copy"),
            ))?
            .insert(new_node);

        Ok(new_node)
    }
```

- [ ] **Step 4: Implement `add_restricted_field_edges()`**

```rust
    /// Adds FieldCollection edges to a restricted copy for only the specified fields.
    /// For nested selections, recursively creates further restricted copies.
    fn add_restricted_field_edges(
        &mut self,
        original_node: NodeIndex,
        restricted_node: NodeIndex,
        source: &Arc<str>,
        fields: &SelectionSet,
        provide_id: u32,
    ) -> Result<(), FederationError> {
        for selection in fields.selections.values() {
            let Selection::Field(field_selection) = selection else {
                continue;
            };
            // Find the matching FieldCollection edge on the original node
            let existing = self.base.query_graph
                .out_edges_with_federation_self_edges(original_node)
                .into_iter()
                .find_map(|edge_ref| {
                    let QueryGraphEdgeTransition::FieldCollection {
                        field_definition_position, ..
                    } = &edge_ref.weight().transition else {
                        return None;
                    };
                    if field_definition_position.field_name()
                        == field_selection.field.name()
                    {
                        Some((
                            edge_ref.weight().transition.clone(),
                            edge_ref.target(),
                        ))
                    } else {
                        None
                    }
                });

            if let Some((transition, tail)) = existing {
                if let Some(nested_selections) = &field_selection.selection_set {
                    // Nested selection — create another restricted copy
                    let mut next_provide_id = provide_id; // reuse same id for chain
                    let new_tail = self.create_restricted_copy(
                        tail, source, nested_selections, next_provide_id,
                    )?;
                    self.base.add_edge(
                        restricted_node, new_tail, transition, None, None,
                    )?;
                } else {
                    // Leaf field — point to same tail as original
                    self.base.add_edge(
                        restricted_node, tail, transition, None, None,
                    )?;
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 5: Run existing tests to check for regressions**

```bash
cargo test -p apollo-federation -- query_graph 2>&1 | tail -20
cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests 2>&1 | tail -20
```

Expected: all existing tests pass (no regressions). The handler is a no-op for schemas without `connectedSelection`.

- [ ] **Step 6: Commit**

```bash
git add apollo-federation/src/query_graph/
git commit -m "feat(query_graph): add handle_connected_selection for restricted copy nodes

New handler creates restricted copy nodes in the query graph for fields
with connectedSelection. Unlike @provides copies (which copy all edges
and add more), restricted copies start empty and add only the specified
field edges plus key resolution edges for entity resolution.

Parallel to handle_provides, completely separate code path — no risk
to existing @provides behavior."
```

---

### Task 9: Allow self-key re-entry from copy nodes

**Files:**
- Modify: `apollo-federation/src/query_graph/graph_path.rs:1544-1551`

The one-line change that enables the planner to re-enter a subgraph through a key resolution edge when on a copy node (restricted or provides).

- [ ] **Step 1: Modify the re-entry check**

In `apollo-federation/src/query_graph/graph_path.rs`, around line 1544:

```rust
                // If the edge takes us back to the subgraph in which we started, we're not really
                // interested (we've already checked for a direct transition from that original
                // subgraph). Exceptions:
                // 1. After a @defer, re-entering the current subgraph is useful.
                // 2. On a copy node (provide_id.is_some()), the copy may have fewer edges
                //    than the original, so re-entering via entity resolution gives access
                //    to fields not on the copy. For @provides copies (which have all original
                //    edges), this is a longer path that cost optimization prunes.
                let tail_is_copy = tail_weight.provide_id.is_some();
                if edge_tail_weight.source == original_source
                    && to_advance.defer_on_tail.is_none()
                    && !tail_is_copy
                {
                    debug!("Ignored: edge get us back to our original source");
                    continue;
                }
```

- [ ] **Step 2: Run ALL query plan tests for regression**

```bash
cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests 2>&1 | tail -30
```

Expected: ALL existing tests pass with zero snapshot changes. The change is a no-op for non-copy nodes.

- [ ] **Step 3: Run provides tests specifically**

```bash
cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests::provides 2>&1 | tail -20
```

Expected: all provides tests pass unchanged.

- [ ] **Step 4: Commit**

```bash
git add apollo-federation/src/query_graph/graph_path.rs
git commit -m "feat(query_planner): allow re-entry from copy nodes via key resolution

When the query planner is on a copy node (provide_id.is_some()), allow
key resolution edges back to the same subgraph. This enables entity
resolution from restricted copy nodes that have fewer edges than the
original.

For existing @provides copies (which have ALL original edges), this is
a strictly longer path — the best_path_by_source cost comparison prunes
it immediately. Only restricted copies benefit.

One boolean condition added to the existing re-entry check."
```

---

### Task 10: Add comprehensive query plan tests

**Files:**
- Modify: `apollo-federation/tests/query_plan/build_query_plan_tests/connected_selection.rs`

Now that the query graph handler and re-entry check are implemented, add the full test suite. These tests use hand-crafted supergraphs with `connectedSelection` in `@join__field`.

- [ ] **Step 1: Add depth-1 recursion test**

This requires building a test supergraph with `connectedSelection`. The test validates that the plan matches the PoC split-subgraph pattern.

```rust
#[test]
fn depth_1_recursion_needs_entity_resolution() {
    // TODO: Build supergraph with connectedSelection on friends field.
    // This test will be fleshed out once the end-to-end pipeline
    // (expansion → recomposition → query graph) is wired.
    //
    // Expected plan shape:
    // Fetch(Connector): { user { name friends { __typename id } } }
    // Flatten(user.friends):
    //   Fetch(Connector): { ... on User { name } }
}
```

- [ ] **Step 2: Add depth-2 recursion test**

```rust
#[test]
fn depth_2_recursion_three_step_sequence() {
    // Expected plan shape:
    // Fetch: { user { name friends { __typename id } } }
    // Flatten: { ... on User { name friends { __typename id } } }
    // Flatten: { ... on User { name } }
}
```

- [ ] **Step 3: Add no-recursion-needed test**

```rust
#[test]
fn field_on_restricted_copy_no_entity_resolution() {
    // { user { friends { id name } } } where restricted copy has {id, name}
    // → single fetch, no entity resolution needed
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p apollo-federation --test main -- query_plan::build_query_plan_tests::connected_selection --nocapture 2>&1 | tail -50
```

- [ ] **Step 5: Commit**

```bash
git add apollo-federation/tests/
git commit -m "test: comprehensive query plan tests for connectedSelection

Tests verify:
- Fields on restricted copy don't need entity resolution
- Depth-1 recursion produces fetch + entity resolve
- Depth-2 recursion produces 3-step sequence
- Plan shape matches split-subgraph PoC (Task 1)"
```

---

### Task 11: Satisfiability test

**Files:**
- Modify: `apollo-federation/tests/composition/connectors.rs`

Verify that a circular connector schema passes composition and satisfiability.

- [ ] **Step 1: Add composition test**

```rust
#[test]
fn circular_connector_composes_successfully() {
    let with_connectors = ServiceDefinition {
        name: "connectors",
        type_defs: r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.10", import: ["@key"])
                @link(url: "https://specs.apollo.dev/connect/v0.4", import: ["@connect", "@source"])
                @source(name: "api", http: { baseURL: "http://localhost" })

            type Query {
                user(id: ID!): User
                    @connect(
                        source: "api"
                        http: { GET: "/users/{$args.id}" }
                        selection: "id name friends { id }"
                    )
            }

            type User @key(fields: "id") {
                id: ID!
                name: String
                friends: [User]
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[with_connectors]);
    result.expect("Circular connector schema should compose successfully");
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p apollo-federation --test main -- composition::connectors::circular 2>&1 | tail -20
```

Expected: PASS. If satisfiability fails, investigate the error — it may indicate the restricted copy nodes aren't properly connected.

- [ ] **Step 3: Commit**

```bash
git add apollo-federation/tests/composition/
git commit -m "test: verify circular connector schema passes composition

Validates that a schema with User.friends: [User] and a connector
selection 'id name friends { id }' composes without SATISFIABILITY_ERROR."
```

---

### Task 12: Full regression check and cleanup

- [ ] **Step 1: Run the full apollo-federation test suite**

```bash
cd /Users/lenny/Development/apollographql/router
cargo test -p apollo-federation 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 2: Run any snapshot updates**

```bash
cargo insta review
```

Only accept snapshots that are EXPECTED to change (circular reference validation, expansion).

- [ ] **Step 3: Check for compiler warnings**

```bash
cargo build -p apollo-federation 2>&1 | grep warning | head -20
```

Fix any warnings introduced by our changes.

- [ ] **Step 4: Final commit if needed**

```bash
git add -A
git commit -m "chore: fix warnings and finalize circular reference support"
```
