use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;
use super::print_sdl;

// =============================================================================
// DIRECTIVE MERGING - Tests for GraphQL built-in directive merging
// =============================================================================

#[test]
fn directive_merging_propagates_graphql_built_in_directives() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: String @shareable @deprecated(reason: "bad")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          a: String @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      a: String @deprecated(reason: "bad")
    }
    "###);
}

#[test]
fn directive_merging_merges_graphql_built_in_directives() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: String @shareable @deprecated(reason: "bad")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          a: String @shareable @deprecated(reason: "bad")
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      a: String @deprecated(reason: "bad")
    }
    "###);
}

#[test]
fn directive_merging_propagates_built_in_directives_even_if_redefined() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: String @deprecated
        }

        # Do note that the code validates that this definition below
        # is "compatible" with the "real one", which it is.
        directive @deprecated on FIELD_DEFINITION
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          b: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      a: String @deprecated
      b: String
    }
    "###);
}

// =============================================================================
// FIELD SHARING - Tests for @shareable directive validation
// =============================================================================

#[test]
fn field_sharing_errors_if_non_shareable_fields_shared_in_value_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: A
        }

        type A {
          x: Int
          y: Int
          z: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A {
          x: Int
          z: Int @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[
            (
                "INVALID_FIELD_SHARING",
                r#"Non-shareable field "A.x" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in all of them"#,
            ),
            (
                "INVALID_FIELD_SHARING",
                r#"Non-shareable field "A.z" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in subgraph "subgraphA""#,
            ),
        ],
    );
}

#[test]
fn field_sharing_errors_if_non_shareable_fields_shared_in_entity_type() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: A
        }

        type A @key(fields: "x") {
          x: Int
          y: Int
          z: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A @key(fields: "x") {
          x: Int
          z: Int @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Non-shareable field "A.z" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in subgraph "subgraphA""#,
        )],
    );
}

#[test]
fn field_sharing_errors_if_query_shared_without_shareable() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          me: String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          me: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Non-shareable field "Query.me" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in all of them"#,
        )],
    );
}

#[test]
fn field_sharing_errors_if_provided_fields_not_marked_shareable() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
          type Query {
            e: E
          }

          type E @key(fields: "id") {
            id: ID!
            a: Int
            b: Int
            c: Int
          }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
          type Query {
            eWithProvided: E @provides(fields: "a c")
          }

          type E @key(fields: "id") {
            id: ID!
            a: Int @external
            c: Int @external
            d: Int
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[
            (
                "INVALID_FIELD_SHARING",
                r#"Non-shareable field "E.a" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in subgraph "subgraphA""#,
            ),
            (
                "INVALID_FIELD_SHARING",
                r#"Non-shareable field "E.c" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in subgraph "subgraphA""#,
            ),
        ],
    );
}

#[test]
fn field_sharing_applies_shareable_on_type_only_to_fields_within_definition() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: A
        }

        type A @shareable {
          x: Int
          y: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A {
          x: Int
        }

        extend type A {
          z: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Non-shareable field "A.x" is resolved from multiple subgraphs"#,
        )],
    );
}

#[test]
fn field_sharing_include_hint_in_error_for_targetless_override() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
          type Query {
            e: E
          }

          type E @key(fields: "id") {
            id: ID!
            a: Int @override(from: "badName")
          }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
          type E @key(fields: "id") {
            id: ID!
            a: Int
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Non-shareable field "E.a" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in all of them (please note that "E.a" has an @override directive in "subgraphA" that targets an unknown subgraph so this could be due to misspelling the @override(from:) argument)"#,
        )],
    );
}

#[test]
fn field_sharing_allows_shareable_on_type_definition_and_extensions() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
          type Query {
            e: E
          }

          type E @shareable {
            id: ID!
            a: Int
          }

          extend type E @shareable {
            b: Int
          }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
          type E @shareable {
            id: ID!
            a: Int
            b: Int
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    // Note that a previous test makes sure that _not_ having @shareable on the type extension ends up failing (as `b` is
    // not considered shareable in `subgraphA`. So succeeding here shows both that @shareable is accepted in the 2 places
    // (definition and extension) but also that it's properly taking into account.
    let _supergraph = result.expect("Expected composition to succeed");
}

#[test]
fn interface_object_field_requires_shareable() {
    // Subgraph A: Defines the interface and a concrete type that implements it
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        interface Node @key(fields: "id") {
            id: ID!
        }
        
        type Entity implements Node @key(fields: "sku") @key(fields: "id") {
            sku: ID!
            id: ID!
            name: String
        }
        
        type Query {
            entity(id: ID!): Entity
        }
        "#,
    };

    // Subgraph B: Uses @interfaceObject to add a field to the interface
    // The sku field is NOT a key field on the interface object
    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Node @key(fields: "id") @interfaceObject {
            sku: ID!
            id: ID!
        }
        
        type RelatedData @key(fields: "node { id }") {
            node: Node!
            metadata: String
        }
        
        type Query {
            related(id: ID!): RelatedData
        }
        "#,
    };

    // Subgraph C: Another implementation that resolves sku
    let subgraph_c = ServiceDefinition {
        name: "subgraphC",
        type_defs: r#"
        type Entity @key(fields: "sku") {
            sku: ID!
            description: String
        }
        "#,
    };

    // This should fail because:
    // 1. Entity.sku is resolved from subgraphA and subgraphC
    // 2. Entity.sku is also accessible through the @interfaceObject in subgraphB
    // 3. The sku field on the interface object is NOT a key field (key is "id")
    // 4. Therefore, sku should be marked @shareable but it's not
    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Non-shareable field "Entity.sku" is resolved from multiple subgraphs"#,
        )],
    );
}

#[test]
fn interface_object_key_field_is_shareable() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        interface Node @key(fields: "id") {
            id: ID!
        }

        type Entity implements Node @key(fields: "id") {
            id: ID!
            name: String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Node @key(fields: "id") @interfaceObject {
            id: ID!
        }

        type Query {
            node(id: ID!): Node
        }
        "#,
    };

    let subgraph_c = ServiceDefinition {
        name: "subgraphC",
        type_defs: r#"
        type Entity @key(fields: "id") {
            id: ID!
            description: String
        }
        "#,
    };

    // This should succeed because id is a key field on all types
    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
    assert!(
        result.is_ok(),
        "Expected composition to succeed when interface object key fields are shared"
    );
}

// =============================================================================
// FEDERATION DIRECTIVE RENAMING - Tests for renamed federation directives
// =============================================================================

#[test]
fn federation_directive_handles_renamed_federation_directives() {
    let subgraph_a = Subgraph::parse(
        "subgraphA", 
        "http://subgraphA",
        r#"
        extend schema @link(
          url: "https://specs.apollo.dev/federation/v2.0",
          import: [{ name: "@key", as: "@identity"}, {name: "@requires", as: "@gimme"}, {name: "@external", as: "@notInThisSubgraph"}]
        )

        type Query {
          users: [User]
        }

        type User @identity(fields: "id") {
          id: ID!
          name: String!
          birthdate: String! @notInThisSubgraph
          age: Int! @gimme(fields: "birthdate")
        }
        "#,
    ).expect("subgraphA should parse successfully");

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "http://subgraphB",
        r#"
        extend schema @link(
          url: "https://specs.apollo.dev/federation/v2.0",
          import: [{ name: "@key", as: "@myKey"}]
        )

        type User @myKey(fields: "id") {
          id: ID!
          birthdate: String!
        }
        "#,
    )
    .expect("subgraphB should parse successfully");

    let result = compose(vec![subgraph_a, subgraph_b]);
    let supergraph =
        result.expect("Expected composition to succeed with renamed federation directives");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      users: [User]
    }

    type User {
      id: ID!
      name: String!
      birthdate: String!
      age: Int!
    }
    "###);
}

#[test]
fn composition_with_shareable_on_interface_object_field() {
    // This test reproduces a bug where during interfaceObject field backfilling, we were blindly
    // copying AST nodes from the subgraph. This sometimes wrongly copied subgraph-only directives
    // to the supergraph.

    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "http://subgraphA",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.6", import: ["@key", "@shareable"])

        type Query {
          item: Item
        }

        interface Item @key(fields: "id") {
          id: ID!
          name: String!
        }

        type Product implements Item @key(fields: "id") {
          id: ID!
          name: String!
          price: Float
        }
        "#,
    )
    .expect("subgraphA should parse successfully");

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "http://subgraphB",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.6", import: ["@key"])

        type Item @federation__interfaceObject @key(fields: "id") {
          id: ID!
          description: String! @federation__shareable
        }
        "#,
    )
    .expect("subgraphB should parse successfully");

    let result = compose(vec![subgraph_a, subgraph_b]);
    // We should have skipped copying the @federation__shareable on Item.description
    let supergraph = result.expect("Expected composition to succeed");
    assert!(
        !supergraph
            .schema()
            .schema()
            .to_string()
            .contains("federation__shareable")
    );
}
