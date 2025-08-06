// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: '@tag'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

mod propagates_tag_to_supergraph {
    use super::*;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn works_for_fed2_subgraphs() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  users: [User] @tag(name: "aTaggedOperation")
                }

                type User @key(fields: "id") {
                  id: ID!
                  name: String! @tag(name: "aTaggedField")
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type User @key(fields: "id") @tag(name: "aTaggedType") {
                  id: ID!
                  birthdate: String!
                  age: Int!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = assert_composition_success(&result);

        assert_api_schema_snapshot(supergraph);
    }
}

mod merges_multiple_tag_on_element {
    use super::*;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn works_for_fed2_subgraphs() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  user: [User]
                }

                type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphA") @tag(name: "aMergedTagOnType") {
                  id: ID!
                  name1: Name!
                }

                type Name {
                  firstName: String @tag(name: "aTagOnFieldFromSubgraphA")
                  lastName: String @tag(name: "aMergedTagOnField")
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphB") @tag(name: "aMergedTagOnType") {
                  id: ID!
                  name2: String!
                }

                type Name {
                  firstName: String @tag(name: "aTagOnFieldFromSubgraphB")
                  lastName: String @tag(name: "aMergedTagOnField")
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = assert_composition_success(&result);

        assert_api_schema_snapshot(supergraph);
    }
}

mod rejects_tag_and_external_together {
    use super::*;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn works_for_fed2_subgraphs() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  user: [User]
                }

                type User @key(fields: "id") {
                  id: ID!
                  name: String!
                  birthdate: Int! @external @tag(name: "myTag")
                  age: Int! @requires(fields: "birthdate")
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type User @key(fields: "id") {
                  id: ID!
                  birthdate: Int!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            vec![
                "[subgraphA] Cannot apply merged directive @tag(name: \"myTag\") to external field \"User.birthdate\""
            ]
        );
    }
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_out_if_tag_is_imported_under_mismatched_names() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

            type Query {
              q1: Int @apolloTag(name: "t1")
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            type Query {
              q2: Int @tag(name: "t2")
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let messages = error_messages(&result);
    assert_eq!(
        messages,
        vec![
            "The \"@tag\" directive (from https://specs.apollo.dev/federation/v2.0) is imported with mismatched name between subgraphs: it is imported as \"@tag\" in subgraph \"subgraphB\" but \"@apolloTag\" in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn succeeds_if_tag_is_imported_under_same_non_default_name() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

            type Query {
              q1: Int @apolloTag(name: "t1")
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

            type Query {
              q2: Int @apolloTag(name: "t2")
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

// TODO: Add Fed1/Fed2 compatibility tests for @tag directive
// Add tests that verify @tag directive propagation works correctly when mixing
// Federation v1 and v2 subgraphs:
// - "works for fed1 subgraphs" - Test @tag with fed1 subgraphs
// - "works for mixed fed1/fed2 subgraphs" - Test @tag with mixed federation versions
// - "merges multiple @tag on an element" with fed1/fed2 compatibility
// These tests should ensure that @tag directive merging works correctly regardless
// of federation version combinations.
