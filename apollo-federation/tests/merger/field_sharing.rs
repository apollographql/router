// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'field sharing'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_non_shareable_fields_are_shared_in_value_types() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in all of them",
            "Non-shareable field \"A.z\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_non_shareable_fields_are_shared_in_entity_type() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Non-shareable field \"A.z\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_a_query_is_shared_without_shareable() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Non-shareable field \"Query.me\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in all of them"
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_provided_fields_are_not_marked_shareable() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Non-shareable field \"E.a\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphA\"",
            "Non-shareable field \"E.c\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn applies_shareable_on_type_only_to_field_within_definition() {
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

        extend type E {
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
    // We want the @shareable to only apply to `a` but not `b` in the first
    // subgraph, so this should _not_ compose.
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Non-shareable field \"E.b\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphA\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn include_hint_in_error_message_on_shareable_error_due_to_target_less_override() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Non-shareable field \"E.a\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in all of them (please note that \"E.a\" has an @override directive in \"subgraphA\" that targets an unknown subgraph so this could be due to misspelling the @override(from:) argument)"
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn allows_applying_shareable_on_both_type_definition_and_extensions() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

mod interface_object {
    use super::*;

    // An @interfaceObject type provides fields for all the implementation it abstracts, which should impact the shareability
    // for those concrete implementations. That is, if a field is provided by both an @interfaceObject and also by one concrete
    // implementation in another subgraph, then it needs to be marked @shareable. Those test check this as well as some
    // variants.

    #[derive(Debug)]
    struct ShareabilityTestCase {
        name: &'static str,
        interface_object_shareable: bool,
        concrete_type_shareable: bool,
        expected_error: Option<&'static str>,
    }

    #[derive(Debug)]
    struct DualInterfaceTestCase {
        name: &'static str,
        first_interface_shareable: bool,
        second_interface_shareable: bool,
        expected_error: Option<&'static str>,
    }

    fn create_single_interface_subgraphs(
        interface_shareable: bool,
        concrete_shareable: bool,
    ) -> (ServiceDefinition<'static>, ServiceDefinition<'static>) {
        let interface_shareable_attr = if interface_shareable {
            " @shareable"
        } else {
            ""
        };
        let concrete_shareable_attr = if concrete_shareable {
            " @shareable"
        } else {
            ""
        };

        let subgraph_a_type_defs = format!(
            r#"
            type Query {{
                iFromA: I
            }}

            type I @interfaceObject @key(fields: "id") {{
                id: ID!
                x: Int{interface_shareable_attr}
            }}
            "#
        );

        let subgraph_b_type_defs = format!(
            r#"
            type Query {{
                iFromB: I
            }}

            interface I @key(fields: "id") {{
                id: ID!
                x: Int
            }}

            type A implements I @key(fields: "id") {{
                id: ID!
                x: Int{concrete_shareable_attr}
            }}
            "#
        );

        // We need to box the strings to have static lifetime
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: Box::leak(subgraph_a_type_defs.into_boxed_str()),
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: Box::leak(subgraph_b_type_defs.into_boxed_str()),
        };

        (subgraph_a, subgraph_b)
    }

    fn create_dual_interface_subgraphs(
        first_shareable: bool,
        second_shareable: bool,
    ) -> (ServiceDefinition<'static>, ServiceDefinition<'static>) {
        let first_shareable_attr = if first_shareable { " @shareable" } else { "" };
        let second_shareable_attr = if second_shareable { " @shareable" } else { "" };

        let subgraph_a_type_defs = format!(
            r#"
            type Query {{
                i1FromA: I1
                i2FromA: I2
            }}

            type I1 @interfaceObject @key(fields: "id") {{
                id: ID!
                x: Int{first_shareable_attr}
            }}

            type I2 @interfaceObject @key(fields: "id") {{
                id: ID!
                x: Int{second_shareable_attr}
            }}
            "#
        );

        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: Box::leak(subgraph_a_type_defs.into_boxed_str()),
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
            type Query {
                i1FromB: I1
                i2FromB: I2
            }

            interface I1 @key(fields: "id") {
                id: ID!
            }

            interface I2 @key(fields: "id") {
                id: ID!
            }

            type A implements I1 & I2 @key(fields: "id") {
                id: ID!
            }
            "#,
        };

        (subgraph_a, subgraph_b)
    }

    fn run_shareability_test(test_case: &ShareabilityTestCase) {
        let (subgraph_a, subgraph_b) = create_single_interface_subgraphs(
            test_case.interface_object_shareable,
            test_case.concrete_type_shareable,
        );

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);

        match test_case.expected_error {
            Some(expected_error) => {
                assert!(
                    result.is_err(),
                    "Test '{}' should have failed",
                    test_case.name
                );
                let errors = error_messages(&result);
                assert_eq!(
                    errors,
                    vec![expected_error],
                    "Test '{}' error mismatch",
                    test_case.name
                );
            }
            None => {
                let supergraph = assert_composition_success(&result);
                assert_api_schema_snapshot(supergraph);
            }
        }
    }

    fn run_dual_interface_test(test_case: &DualInterfaceTestCase) {
        let (subgraph_a, subgraph_b) = create_dual_interface_subgraphs(
            test_case.first_interface_shareable,
            test_case.second_interface_shareable,
        );

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);

        match test_case.expected_error {
            Some(expected_error) => {
                assert!(
                    result.is_err(),
                    "Test '{}' should have failed",
                    test_case.name
                );
                let errors = error_messages(&result);
                assert_eq!(
                    errors,
                    vec![expected_error],
                    "Test '{}' error mismatch",
                    test_case.name
                );
            }
            None => {
                let supergraph = assert_composition_success(&result);
                assert_api_schema_snapshot(supergraph);
            }
        }
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn enforces_shareable_constraints_for_field_abstracted_by_interface_object_and_shared() {
        let test_cases = [
            ShareabilityTestCase {
                name: "concrete_type_false_interface_object_false",
                interface_object_shareable: false,
                concrete_type_shareable: false,
                expected_error: Some(
                    "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" (through @interfaceObject field \"I.x\") and \"subgraphB\" and defined as non-shareable in all of them",
                ),
            },
            ShareabilityTestCase {
                name: "concrete_type_true_interface_object_false",
                interface_object_shareable: false,
                concrete_type_shareable: true,
                expected_error: Some(
                    "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" (through @interfaceObject field \"I.x\") and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphA\" (through @interfaceObject field \"I.x\")",
                ),
            },
            ShareabilityTestCase {
                name: "concrete_type_false_interface_object_true",
                interface_object_shareable: true,
                concrete_type_shareable: false,
                expected_error: Some(
                    "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" (through @interfaceObject field \"I.x\") and \"subgraphB\" and defined as non-shareable in subgraph \"subgraphB\"",
                ),
            },
            ShareabilityTestCase {
                name: "concrete_type_true_interface_object_true",
                interface_object_shareable: true,
                concrete_type_shareable: true,
                expected_error: None,
            },
        ];

        for test_case in &test_cases {
            run_shareability_test(test_case);
        }
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn enforces_shareability_in_single_subgraph_with_2_intersecting_interface_objects() {
        let test_cases = [
            DualInterfaceTestCase {
                name: "first_interface_false_second_interface_false",
                first_interface_shareable: false,
                second_interface_shareable: false,
                expected_error: Some(
                    "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" (through @interfaceObject field \"I1.x\") and \"subgraphA\" (through @interfaceObject field \"I2.x\") and defined as non-shareable in all of them",
                ),
            },
            DualInterfaceTestCase {
                name: "first_interface_true_second_interface_false",
                first_interface_shareable: true,
                second_interface_shareable: false,
                expected_error: Some(
                    "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" (through @interfaceObject field \"I1.x\") and \"subgraphA\" (through @interfaceObject field \"I2.x\") and defined as non-shareable in subgraph \"subgraphA\" (through @interfaceObject field \"I2.x\")",
                ),
            },
            DualInterfaceTestCase {
                name: "first_interface_false_second_interface_true",
                first_interface_shareable: false,
                second_interface_shareable: true,
                expected_error: Some(
                    "Non-shareable field \"A.x\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" (through @interfaceObject field \"I1.x\") and \"subgraphA\" (through @interfaceObject field \"I2.x\") and defined as non-shareable in subgraph \"subgraphA\" (through @interfaceObject field \"I1.x\")",
                ),
            },
            DualInterfaceTestCase {
                name: "first_interface_true_second_interface_true",
                first_interface_shareable: true,
                second_interface_shareable: true,
                expected_error: None,
            },
        ];

        for test_case in &test_cases {
            run_dual_interface_test(test_case);
        }
    }
}
