// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'Input types'

use super::ServiceDefinition;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;
use crate::merger::assert_api_schema_snapshot;

#[ignore = "until merge implementation completed"]
#[test]
fn only_merges_fields_common_to_all_subgraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            q1(a: A): String
        }

        input A {
            x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
            q2(a: A): String
        }

        input A {
            x: Int
            y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_input_field_with_different_but_compatible_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            q1(a: A): String
        }

        input A {
            x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
            q2(a: A): String
        }

        input A {
            x: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_when_merging_completely_inconsistent_input_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            f(i: MyInput!): Int
        }

        input MyInput {
            x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        input MyInput {
            y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "None of the fields of input object type \"MyInput\" are consistently defined in all the subgraphs defining that type. As only fields common to all subgraphs are merged, this would result in an empty type."
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_a_mandatory_input_field_is_not_in_all_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            q1(a: A): String
        }

        input A {
            x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
            q2(a: A): String
        }

        input A {
            x: Int
            y: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Input object field \"A.y\" is required in some subgraphs but does not appear in all subgraphs: it is required in subgraph \"subgraphB\" but does not appear in subgraph \"subgraphA\""
        ]
    );
}
