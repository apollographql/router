// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: '@requiresScopes and @policy'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

#[derive(Debug, Clone)]
struct DirectiveTestCase {
    directive_name: &'static str,
    arg_name: &'static str,
}

const TESTS_TO_RUN: [DirectiveTestCase; 2] = [
    DirectiveTestCase {
        directive_name: "@requiresScopes",
        arg_name: "scopes",
    },
    DirectiveTestCase {
        directive_name: "@policy",
        arg_name: "policies",
    },
];

fn create_comprehensive_locations_test(
    test_case: &DirectiveTestCase,
) -> Vec<ServiceDefinition<'static>> {
    let directive_name = test_case.directive_name;
    let arg_name = test_case.arg_name;

    let on_object = ServiceDefinition {
        name: "on-object",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                object: ScopedObject!
            }}

            type ScopedObject {}({}: ["object"]) {{
                field: Int!
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let on_interface = ServiceDefinition {
        name: "on-interface",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                interface: ScopedInterface!
            }}

            interface ScopedInterface {}({}: ["interface"]) {{
                field: Int!
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let on_interface_object = ServiceDefinition {
        name: "on-interface-object",
        type_defs: Box::leak(
            format!(
                r#"
            type ScopedInterfaceObject
                @interfaceObject
                @key(fields: "id")
                {}({}: ["interfaceObject"])
            {{
                id: String!
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: Box::leak(
            format!(
                r#"
            scalar ScopedScalar {}({}: ["scalar"])

            # This needs to exist in at least one other subgraph from where it's defined
            # as an @interfaceObject (so arbitrarily adding it here). We don't actually
            # apply {} to this one since we want to see it propagate even
            # when it's not applied in all locations.
            interface ScopedInterfaceObject @key(fields: "id") {{
                id: String!
            }}
            "#,
                directive_name, arg_name, directive_name
            )
            .into_boxed_str(),
        ),
    };

    let on_enum = ServiceDefinition {
        name: "on-enum",
        type_defs: Box::leak(
            format!(
                r#"
            enum ScopedEnum {}({}: ["enum"]) {{
                A
                B
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let on_root_field = ServiceDefinition {
        name: "on-root-field",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                scopedRootField: Int! {}({}: ["rootField"])
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let on_object_field = ServiceDefinition {
        name: "on-object-field",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                objectWithField: ObjectWithScopedField!
            }}

            type ObjectWithScopedField {{
                field: Int! {}({}: ["objectField"])
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let on_entity_field = ServiceDefinition {
        name: "on-entity-field",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                entityWithField: EntityWithScopedField!
            }}

            type EntityWithScopedField @key(fields: "id") {{
                id: ID!
                field: Int! {}({}: ["entityField"])
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    vec![
        on_object,
        on_interface,
        on_interface_object,
        on_scalar,
        on_enum,
        on_root_field,
        on_object_field,
        on_entity_field,
    ]
}

#[ignore = "until merge implementation completed"]
#[test]
fn comprehensive_locations() {
    for test_case in &TESTS_TO_RUN {
        let subgraphs = create_comprehensive_locations_test(test_case);
        let result = compose_as_fed2_subgraphs(&subgraphs);
        let supergraph = assert_composition_success(&result);

        // Note: In the TypeScript version, they check that each element has the directive applied
        // For now, we'll just ensure composition succeeds and take a snapshot
        assert_api_schema_snapshot(supergraph);
    }
}

fn create_applies_directive_test(
    test_case: &DirectiveTestCase,
) -> (ServiceDefinition<'static>, ServiceDefinition<'static>) {
    let directive_name = test_case.directive_name;
    let arg_name = test_case.arg_name;

    let a1 = ServiceDefinition {
        name: "a1",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                a: A
            }}
            type A @key(fields: "id") {}({}: ["a"]) {{
                id: String!
                a1: String
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let a2 = ServiceDefinition {
        name: "a2",
        type_defs: r#"
            type A @key(fields: "id") {
                id: String!
                a2: String
            }
        "#,
    };

    (a1, a2)
}

#[ignore = "until merge implementation completed"]
#[test]
fn applies_directive_on_types_as_long_as_it_is_used_once() {
    for test_case in &TESTS_TO_RUN {
        let (a1, a2) = create_applies_directive_test(test_case);

        // checking composition in either order (not sure if this is necessary but
        // it's not hurting anything)
        let result1 = compose_as_fed2_subgraphs(&[a1.clone(), a2.clone()]);
        let result2 = compose_as_fed2_subgraphs(&[a2, a1]);
        assert_composition_success(&result1);
        assert_composition_success(&result2);

        // Note: In the TypeScript version, they check that the directive is applied to type 'A'
        // For now, we'll just ensure composition succeeds
    }
}

fn create_merges_lists_simple_union_test(
    test_case: &DirectiveTestCase,
) -> (ServiceDefinition<'static>, ServiceDefinition<'static>) {
    let directive_name = test_case.directive_name;
    let arg_name = test_case.arg_name;

    let a1 = ServiceDefinition {
        name: "a1",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                a: A
            }}

            type A {}({}: ["a"]) @key(fields: "id") {{
                id: String!
                a1: String
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let a2 = ServiceDefinition {
        name: "a2",
        type_defs: Box::leak(
            format!(
                r#"
            type A {}({}: ["b"]) @key(fields: "id") {{
                id: String!
                a2: String
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    (a1, a2)
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_lists_simple_union() {
    for test_case in &TESTS_TO_RUN {
        let (a1, a2) = create_merges_lists_simple_union_test(test_case);

        let result = compose_as_fed2_subgraphs(&[a1, a2]);
        let supergraph = assert_composition_success(&result);

        // Note: In the TypeScript version, they check that the merged list contains ['a', 'b']
        // For now, we'll just ensure composition succeeds and take a snapshot
        assert_api_schema_snapshot(supergraph);
    }
}

fn create_merges_lists_deduplicates_intersecting_test(
    test_case: &DirectiveTestCase,
) -> (ServiceDefinition<'static>, ServiceDefinition<'static>) {
    let directive_name = test_case.directive_name;
    let arg_name = test_case.arg_name;

    let a1 = ServiceDefinition {
        name: "a1",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                a: A
            }}

            type A {}({}: ["a", "b"]) @key(fields: "id") {{
                id: String!
                a1: String
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let a2 = ServiceDefinition {
        name: "a2",
        type_defs: Box::leak(
            format!(
                r#"
            type A {}({}: ["b", "c"]) @key(fields: "id") {{
                id: String!
                a2: String
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    (a1, a2)
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_lists_deduplicates_intersecting() {
    for test_case in &TESTS_TO_RUN {
        let (a1, a2) = create_merges_lists_deduplicates_intersecting_test(test_case);

        let result = compose_as_fed2_subgraphs(&[a1, a2]);
        let supergraph = assert_composition_success(&result);

        // Note: In the TypeScript version, they check that the merged list contains ['a', 'b', 'c']
        // For now, we'll just ensure composition succeeds and take a snapshot
        assert_api_schema_snapshot(supergraph);
    }
}

fn create_has_correct_definition_test(test_case: &DirectiveTestCase) -> ServiceDefinition<'static> {
    let directive_name = test_case.directive_name;
    let arg_name = test_case.arg_name;

    ServiceDefinition {
        name: "a",
        type_defs: Box::leak(
            format!(
                r#"
            type Query {{
                x: Int {}({}: ["a", "b"])
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    }
}

#[ignore = "until merge implementation completed"]
#[test]
fn directive_has_correct_definition_in_supergraph() {
    for test_case in &TESTS_TO_RUN {
        let a = create_has_correct_definition_test(test_case);

        let result = compose_as_fed2_subgraphs(&[a]);
        let supergraph = assert_composition_success(&result);

        // Note: In the TypeScript version, they check the core features and directive definition
        // For now, we'll just ensure composition succeeds and take a snapshot
        assert_api_schema_snapshot(supergraph);
    }
}

fn create_composes_with_existing_scalar_test(
    test_case: &DirectiveTestCase,
) -> (ServiceDefinition<'static>, ServiceDefinition<'static>) {
    let directive_name = test_case.directive_name;
    let arg_name = test_case.arg_name;

    let a = ServiceDefinition {
        name: "a",
        type_defs: Box::leak(
            format!(
                r#"
            scalar Scope
            type Query {{
                x: Int {}({}: ["a", "b"])
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    let b = ServiceDefinition {
        name: "b",
        type_defs: Box::leak(
            format!(
                r#"
            scalar Scope @specifiedBy(url: "not-the-apollo-spec")
            type Query {{
                y: Int {}({}: ["a", "b"])
            }}
            "#,
                directive_name, arg_name
            )
            .into_boxed_str(),
        ),
    };

    (a, b)
}

#[ignore = "until merge implementation completed"]
#[test]
fn composes_with_existing_scope_scalar_definitions_in_subgraphs() {
    for test_case in &TESTS_TO_RUN {
        let (a, b) = create_composes_with_existing_scalar_test(test_case);

        let result = compose_as_fed2_subgraphs(&[a, b]);
        assert_composition_success(&result);
    }
}

// Validation errors tests - moved from validation_errors.rs to match TypeScript structure
mod validation_errors {
    use super::*;

    fn get_fed_type(directive_name: &str) -> &'static str {
        match directive_name {
            "@requiresScopes" => "federation__Scope",
            "@policy" => "federation__Policy",
            _ => panic!("Unknown directive: {}", directive_name),
        }
    }

    fn create_incompatible_directive_location_test(
        test_case: &DirectiveTestCase,
    ) -> ServiceDefinition<'static> {
        let directive_name = test_case.directive_name;
        let arg_name = test_case.arg_name;
        let fed_type = get_fed_type(directive_name);

        ServiceDefinition {
            name: "invalidDefinition",
            type_defs: Box::leak(
                format!(
                    r#"
                    scalar {}
                    directive {}({}: [[{}!]!]!) on ENUM_VALUE

                    type Query {{
                        a: Int
                    }}

                    enum E {{
                        A {}({}: [])
                    }}
                    "#,
                    fed_type, directive_name, arg_name, fed_type, directive_name, arg_name
                )
                .into_boxed_str(),
            ),
        }
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn on_incompatible_directive_location() {
        for test_case in &TESTS_TO_RUN {
            let invalid_definition = create_incompatible_directive_location_test(test_case);

            let result = compose_as_fed2_subgraphs(&[invalid_definition]);
            assert!(result.is_err());

            let errors = error_messages(&result);
            assert!(errors.iter().any(|error| error.contains(&format!(
                "Invalid definition for directive \"{}\"",
                test_case.directive_name
            ))));
        }
    }

    fn create_incompatible_args_test(test_case: &DirectiveTestCase) -> ServiceDefinition<'static> {
        let directive_name = test_case.directive_name;
        let arg_name = test_case.arg_name;
        let fed_type = get_fed_type(directive_name);

        ServiceDefinition {
            name: "invalidDefinition",
            type_defs: Box::leak(
                format!(
                    r#"
                    scalar {}
                    directive {}({}: [{}]!) on FIELD_DEFINITION

                    type Query {{
                        a: Int
                    }}

                    enum E {{
                        A {}({}: [])
                    }}
                    "#,
                    fed_type, directive_name, arg_name, fed_type, directive_name, arg_name
                )
                .into_boxed_str(),
            ),
        }
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn on_incompatible_args() {
        for test_case in &TESTS_TO_RUN {
            let invalid_definition = create_incompatible_args_test(test_case);

            let result = compose_as_fed2_subgraphs(&[invalid_definition]);
            assert!(result.is_err());

            let errors = error_messages(&result);
            assert!(errors.iter().any(|error| error.contains(&format!(
                "Invalid definition for directive \"{}\"",
                test_case.directive_name
            ))));
        }
    }

    fn create_invalid_application_test(
        test_case: &DirectiveTestCase,
    ) -> ServiceDefinition<'static> {
        let directive_name = test_case.directive_name;
        let arg_name = test_case.arg_name;

        ServiceDefinition {
            name: "invalidApplication",
            type_defs: Box::leak(
                format!(
                    r#"
                    type Query {{
                        a: Int
                    }}

                    enum E {{
                        A {}({}: [])
                    }}
                    "#,
                    directive_name, arg_name
                )
                .into_boxed_str(),
            ),
        }
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn on_invalid_application() {
        for test_case in &TESTS_TO_RUN {
            let invalid_application = create_invalid_application_test(test_case);

            let result = compose_as_fed2_subgraphs(&[invalid_application]);
            assert!(result.is_err());

            let errors = error_messages(&result);
            assert!(errors.iter().any(|error| {
                error.contains(&format!(
                    "Directive \"{}\" may not be used on ENUM_VALUE",
                    test_case.directive_name
                ))
            }));
        }
    }
}
