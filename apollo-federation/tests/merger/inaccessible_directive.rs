// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: '@inaccessible'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

#[ignore = "until merge implementation completed"]
#[test]
fn propagates_inaccessible_to_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type Query {
              me: User @inaccessible
              users: [User]
            }

            type User @key(fields: "id") {
              id: ID!
              name: String!
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            type User @key(fields: "id") {
              id: ID!
              birthdate: String!
              age: Int! @inaccessible
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_inaccessible_on_same_element() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type Query {
              user: [User]
            }

            type User @key(fields: "id") {
              id: ID!
              name: String @shareable @inaccessible
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            type User @key(fields: "id") {
              id: ID!
              name: String @shareable @inaccessible
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn rejects_inaccessible_and_external_together() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type Query {
              user: [User]
            }

            type User @key(fields: "id") {
              id: ID!
              name: String!
              birthdate: Int! @external @inaccessible
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
            "[subgraphA] Cannot apply merged directive @inaccessible to external field \"User.birthdate\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_out_if_inaccessible_is_imported_under_mismatched_names() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

            type Query {
              q: Int
              q1: Int @private
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@inaccessible"])

            type Query {
              q2: Int @inaccessible
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let messages = error_messages(&result);
    assert_eq!(
        messages,
        vec![
            "The \"@inaccessible\" directive (from https://specs.apollo.dev/federation/v2.0) is imported with mismatched name between subgraphs: it is imported as \"@inaccessible\" in subgraph \"subgraphB\" but \"@private\" in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn succeeds_if_inaccessible_is_imported_under_same_non_default_name() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

            type Query {
              q: Int
              q1: Int @private
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

            type Query {
              q2: Int @private
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn ignores_inaccessible_element_when_validating_composition() {
    // The following example would _not_ compose if the `z` was not marked inaccessible since it wouldn't be reachable
    // from the `origin` query. So all this test does is double-checking that validation does pass with it marked inaccessible.
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type Query {
              origin: Point
            }

            type Point @shareable {
              x: Int
              y: Int
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            type Point @shareable {
              x: Int
              y: Int
              z: Int @inaccessible
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_subgraph_misuse_inaccessible() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type Query {
              q1: Int
              q2: A
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
            type A @shareable @inaccessible {
              x: Int
              y: Int
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let messages = error_messages(&result);
    assert_eq!(
        messages,
        vec![
            "Type \"A\" is @inaccessible but is referenced by \"Query.q2\", which is in the API schema."
        ]
    );
}
