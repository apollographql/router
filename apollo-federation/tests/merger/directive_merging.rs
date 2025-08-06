// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'merging of directives'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;

#[ignore = "until merge implementation completed"]
#[test]
fn propagates_graphql_built_in_directives() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

// TODO: Add test for "handles renamed federation directives"
// This test should verify that federation directives imported with custom names
// (e.g., @link(import: [{ name: "@key", as: "@identity"}])) are properly processed
// and that the renamed directives (@identity, @gimme, @notInThisSubgraph) work correctly
// in composition. Should test @key/@identity, @requires/@gimme, @external/@notInThisSubgraph.

#[ignore = "until merge implementation completed"]
#[test]
fn merges_graphql_built_in_directives() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

// TODO: Add test for "handles renamed federation directives"
// This test should verify that federation directives imported with custom names
// (e.g., @link(import: [{ name: "@key", as: "@identity"}])) are properly processed
// and that the renamed directives (@identity, @gimme, @notInThisSubgraph) work correctly
// in composition. Should test @key/@identity, @requires/@gimme, @external/@notInThisSubgraph.

#[ignore = "until merge implementation completed"]
#[test]
fn propagates_graphql_built_in_directives_even_if_redefined_in_subgraph() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

// TODO: Add test for "handles renamed federation directives"
// This test should verify that federation directives imported with custom names
// (e.g., @link(import: [{ name: "@key", as: "@identity"}])) are properly processed
// and that the renamed directives (@identity, @gimme, @notInThisSubgraph) work correctly
// in composition. Should test @key/@identity, @requires/@gimme, @external/@notInThisSubgraph.

#[ignore = "until merge implementation completed"]
#[test]
fn is_not_broken_by_similar_field_argument_signatures() {
    // This test is about validating the case from https://github.com/apollographql/federation/issues/1100 is fixed.

    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @shareable {
              a(x: String): Int
              b(x: Int): Int
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
            type T @shareable {
              a(x: String): Int
              b(x: Int): Int
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}
