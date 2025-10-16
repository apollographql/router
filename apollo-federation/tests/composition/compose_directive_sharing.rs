use insta::assert_snapshot;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;
use super::print_sdl;

// =============================================================================
// DIRECTIVE MERGING - Tests for GraphQL built-in directive merging
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
fn field_sharing_errors_if_provided_fields_not_marked_shareable() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          products: [Product]
        }

        type Product @key(fields: "sku") {
          sku: String!
          name: String! @external
          weight: String! @provides(fields: "name")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Product @key(fields: "sku") {
          sku: String!
          name: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Field "Product.name" is provided by subgraph "subgraphA" but is not marked @shareable"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
fn field_sharing_include_hint_in_error_for_targetless_override() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          me: User
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @override(from: "subgraphB")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          name: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_FIELD_SHARING",
            r#"Non-shareable field "User.name" is resolved from multiple subgraphs"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn field_sharing_allows_shareable_on_type_definition_and_extensions() {
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
        type A @shareable {
          x: Int
        }

        extend type A @shareable {
          z: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type A {
      x: Int
      y: Int
      z: String
    }

    type Query {
      a: A
    }
    "###);
}

// =============================================================================
// FEDERATION DIRECTIVE RENAMING - Tests for renamed federation directives
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn federation_directive_handles_renamed_federation_directives() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
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
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        extend schema @link(
          url: "https://specs.apollo.dev/federation/v2.0",
          import: [{ name: "@key", as: "@myKey"}]
        )

        type User @myKey(fields: "id") {
          id: ID!
          birthdate: String!
        }
        "#,
    };

    // Note: This test uses manual federation links, not composeAsFed2Subgraphs
    // TODO: Need to implement equivalent of composeServices for manual @link handling
    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
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
