// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'Enum types'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

#[ignore = "until merge implementation completed"]
#[test]
fn merges_inconsistent_enum_that_are_only_used_as_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            e: E!
        }

        enum E {
            V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_enum_but_skip_inconsistent_enum_values_that_are_only_used_as_input() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            f(e: E!): Int
        }

        enum E {
            V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V1
            V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn do_not_error_if_a_skipped_inconsistent_value_has_directive_applied() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            f(e: E!): Int
        }

        enum E {
            V1
            V2 @deprecated(reason: "use V3 instead")
            V3
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V1
            V3
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_enum_used_only_as_input_as_no_consistent_values() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            f(e: E!): Int
        }

        enum E {
            V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "None of the values of enum type \"E\" are defined consistently in all the subgraphs defining that type. As only values common to all subgraphs are merged, this would result in an empty type."
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_when_merging_inconsistent_enum_that_are_used_as_both_input_and_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            e: E!
            f(e: E!): Int
        }

        enum E {
            V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Enum type \"E\" is used as both input type (for example, as type of \"Query.f(e:)\") and output type (for example, as type of \"Query.e\"), but value \"V1\" is not defined in all the subgraphs defining \"E\": \"V1\" is defined in subgraph \"subgraphA\" but not in subgraph \"subgraphB\"",
            "Enum type \"E\" is used as both input type (for example, as type of \"Query.f(e:)\") and output type (for example, as type of \"Query.e\"), but value \"V2\" is not defined in all the subgraphs defining \"E\": \"V2\" is defined in subgraph \"subgraphB\" but not in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn ignores_inaccessible_fields_when_merging_enums_that_are_used_as_both_input_and_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            e: E!
            f(e: E!): Int
        }

        enum E {
            V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V1
            V2 @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn succeed_merging_consistent_enum_used_as_both_input_and_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            e: E!
            f(e: E!): Int
        }

        enum E {
            V1
            V2
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
            V1
            V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}
