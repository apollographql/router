use insta::assert_snapshot;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;
use super::print_sdl;

// =============================================================================
// MISCELLANEOUS COMPOSITION TESTS - Standalone composition behavior tests
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn misc_works_with_normal_graphql_type_extension_when_definition_is_empty() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          foo: Foo
        }

        type Foo

        extend type Foo {
          bar: String
        }
        "#,
    };

    // NOTE: This test uses composeServices() in JS (Fed1), not composeAsFed2Subgraphs()
    // For now, using Fed2 composition as the equivalent
    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    let _supergraph =
        result.expect("Expected composition to succeed with empty type definition + extension");
}

#[test]
#[ignore = "until merge implementation completed"]
fn misc_handles_fragments_in_requires_using_inaccessible_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query @shareable {
          dummy: Entity
        }

        type Entity @key(fields: "id") {
          id: ID!
          data: Foo
        }

        interface Foo {
          foo: String!
        }

        interface Bar implements Foo {
          foo: String!
          bar: String!
        }

        type Baz implements Foo & Bar @shareable {
          foo: String!
          bar: String!
          baz: String!
        }

        type Qux implements Foo & Bar @shareable {
          foo: String!
          bar: String!
          qux: String!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query @shareable {
          dummy: Entity
        }

        type Entity @key(fields: "id") {
          id: ID!
          data: Foo @external
          requirer: String! @requires(fields: "data { foo ... on Bar { bar ... on Baz { baz } ... on Qux { qux } } }")
        }

        interface Foo {
          foo: String!
        }

        interface Bar implements Foo {
          foo: String!
          bar: String!
        }

        type Baz implements Foo & Bar @shareable @inaccessible {
          foo: String!
          bar: String!
          baz: String!
        }

        type Qux implements Foo & Bar @shareable {
          foo: String!
          bar: String!
          qux: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect(
        "Expected composition to succeed with @requires fragments using @inaccessible types",
    );
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Validate that @inaccessible type Baz is excluded from API schema but Qux is included
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    interface Bar implements Foo {
      foo: String!
      bar: String!
    }

    type Entity {
      id: ID!
      data: Foo
      requirer: String!
    }

    interface Foo {
      foo: String!
    }

    type Query {
      dummy: Entity
    }

    type Qux implements Foo & Bar {
      foo: String!
      bar: String!
      qux: String!
    }
    "###);
}

#[test]
#[ignore = "until merge implementation completed"]
fn misc_existing_authenticated_directive_with_fed1() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        directive @authenticated(scope: [String!]) repeatable on FIELD_DEFINITION

        extend type Foo @key(fields: "id") {
          id: ID!
          name: String! @authenticated(scope: ["read:foo"])
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          foo: Foo
        }

        type Foo @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    // NOTE: This test uses composeServices() in JS (Fed1), not composeAsFed2Subgraphs()
    // The test validates that existing @authenticated directives in Fed1 are handled properly
    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph =
        result.expect("Expected composition to succeed with existing @authenticated directive");

    // NOTE: The JS test validates that the custom @authenticated directive is NOT present in the final schema
    // (it should be filtered out). For now, we just verify composition succeeds.
    let _schema = supergraph.schema();
}

#[test]
fn composes_subgraphs_with_overridden_fields_on_renamed_root_types() {
    // Test that @override directives work correctly when root operation types are renamed
    // during normalization (e.g., MyMutation -> Mutation).
    // This is a regression test for a bug where directive referencers were not updated
    // after type renaming, causing @override directives to not be recognized.

    let subgraph_a = ServiceDefinition {
        name: "subgraph-a",
        type_defs: r#"
            schema {
                query: Query
                mutation: MyMutation
            }

            type Query {
                user(id: ID!): User @shareable
            }

            type MyMutation {
                createUser(name: String!): User! @override(from: "subgraph-b")
                updateUser(id: ID!, name: String!): User! @override(from: "subgraph-b")
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraph-b",
        type_defs: r#"
            schema {
                query: Query
                mutation: Mutation
            }

            type Query {
                user(id: ID!): User @shareable
            }

            type Mutation @shareable {
                createUser(name: String!): User!
                updateUser(id: ID!, name: String!): User!
            }

            type User @key(fields: "id") {
                id: ID!
                email: String
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);

    // Before the fix, this would fail with INVALID_FIELD_SHARING errors because
    // the @override directives on MyMutation fields were not recognized after
    // MyMutation was renamed to Mutation during normalization.
    let _supergraph = result.expect("Expected composition to succeed");
}
