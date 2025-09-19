use std::iter::zip;

use apollo_compiler::ast;
use apollo_compiler::schema;
use apollo_federation::schema::argument_composition_strategies::ArgumentCompositionStrategy;
use apollo_federation::supergraph::CompositionHint;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;
use super::errors;
use super::assert_composition_success;

// Helper function to create directive strings from applied directives
// Note: This function is currently unused but kept for future implementation
fn directive_strings_schema(directives: &schema::DirectiveList, target: &str) -> Vec<String> {
    directives
        .iter()
        .map(|dir| dir.to_string())
        .filter(|s| s.contains(target))
        .collect()
}

fn directive_strings_ast(directives: &ast::DirectiveList, target: &str) -> Vec<String> {
    directives
        .iter()
        .map(|dir| dir.to_string())
        .filter(|s| s.contains(target))
        .collect()
}

fn assert_hints_equal(actual_hints: &Vec<CompositionHint>, expected_hints: &Vec<CompositionHint>) {
    if actual_hints.len() != expected_hints.len() {
        panic!("Mismatched number of hints")
    }
    let zipped = zip(actual_hints, expected_hints);
    zipped
        .for_each(|(ch1, ch2)| assert!(ch1.code() == ch2.code() && ch1.message() == ch2.message()));
}

#[cfg(test)]
mod tests {
    use core::str;
    use std::collections::HashMap;
    use std::sync::LazyLock;

    use super::*;

    // Test cases for different argument composition strategies
    struct CompositionStrategyTestCase<'a> {
        name: &'a str,
        composition_strategy: ArgumentCompositionStrategy,
        arg_values_s1: HashMap<&'a str, &'a str>,
        arg_values_s2: HashMap<&'a str, &'a str>,
        result_values: HashMap<&'a str, &'a str>,
    }

    static TEST_CASES: LazyLock<HashMap<&str, CompositionStrategyTestCase>> = LazyLock::new(|| {
        HashMap::from([
            (
                "max",
                CompositionStrategyTestCase {
                    name: "max",
                    composition_strategy: ArgumentCompositionStrategy::Max,
                    arg_values_s1: HashMap::from([
                        ("t", "3"),
                        ("k", "1")
                    ]),
                    arg_values_s2: HashMap::from([
                        ("t", "2"),
                        ("k", "5"),
                        ("b", "4")
                    ]),
                    result_values: HashMap::from([
                        ("t", "3"),
                        ("k", "5"),
                        ("b", "4")
                    ]),
                },
            ),
            (
                "min",
                CompositionStrategyTestCase {
                    name: "min",
                    composition_strategy: ArgumentCompositionStrategy::Min,
                    arg_values_s1: HashMap::from([
                        ("t", "3"),
                        ("k", "1")
                    ]),
                    arg_values_s2: HashMap::from([
                        ("t", "2"),
                        ("k", "5"),
                        ("b", "4")
                    ]),
                    result_values: HashMap::from([
                        ("t", "2"),
                        ("k", "1"),
                        ("b", "4")
                    ]),
                },
            ),
            (
                "intersection",
                CompositionStrategyTestCase {
                    name: "intersection",
                    composition_strategy: ArgumentCompositionStrategy::Intersection,
                    arg_values_s1:  HashMap::from([
                        ("t", r#"["foo", "bar"]"#),
                        ("k", r#"[]"#)
                    ]),
                    arg_values_s2:  HashMap::from([
                        ("t", r#"["foo"]"#),
                        ("k", r#"["v1", "v2"]"#),
                        ("b", r#"["x"]"#)
                    ]),
                    result_values:  HashMap::from([
                        ("t", r#"["foo"]"#),
                        ("k", r#"[]"#),
                        ("b", r#"["x"]"#)
                    ]),
                },
            ),
            (
                "union",
                CompositionStrategyTestCase {
                    name: "union",
                    composition_strategy: ArgumentCompositionStrategy::Union,
                    arg_values_s1:  HashMap::from([
                        ("t", r#"["foo", "bar"]"#),
                        ("k", r#"[]"#)
                    ]),
                    arg_values_s2:  HashMap::from([
                        ("t", r#"["foo"]"#),
                        ("k", r#"["v1", "v2"]"#),
                        ("b", r#"["x"]"#)
                    ]),
                    result_values:  HashMap::from([
                        ("t", r#"["foo", "bar"]"#),
                        ("k", r#"["v1", "v2"]"#),
                        ("b", r#"["x"]"#)
                    ]),
                },
            ),
            (
                "nullable_and",
                CompositionStrategyTestCase {
                    name: "nullable_and",
                    composition_strategy: ArgumentCompositionStrategy::NullableAnd,
                    arg_values_s1:  HashMap::from([
                        ("t", "true"),
                        ("k", "true")
                    ]),
                    arg_values_s2:  HashMap::from([
                        ("t", "null"),
                        ("k", "false"),
                        ("b", "false")
                    ]),
                    result_values:  HashMap::from([
                        ("t", "true"),
                        ("k", "false"),
                        ("b", "false")
                    ]),
                },
            ),
            (
                "nullable_max",
                CompositionStrategyTestCase {
                    name: "nullable_max",
                    composition_strategy: ArgumentCompositionStrategy::NullableMax,
                    arg_values_s1:  HashMap::from([
                        ("t", "3"),
                        ("k", "1")
                    ]),
                    arg_values_s2:  HashMap::from([
                        ("t", "2"),
                        ("k", "null"),
                        ("b", "null")
                    ]),
                    result_values:  HashMap::from([
                        ("t", "3"),
                        ("k", "1"),
                        ("b", "null")
                    ]),
                },
            ),
            (
                "nullable_union",
                CompositionStrategyTestCase {
                    name: "nullable_union",
                    composition_strategy: ArgumentCompositionStrategy::NullableUnion,
                    arg_values_s1:  HashMap::from([
                        ("t", r#"["foo", "bar"]"#),
                        ("k", r#"[]"#)
                    ]),
                    arg_values_s2:  HashMap::from([
                        ("t", r#"["foo"]"#),
                        ("k", r#"["v1", "v2"]"#),
                        ("b", r#"["x"]"#)
                    ]),
                    result_values:  HashMap::from([
                        ("t", r#"["foo", "bar"]"#),
                        ("k", r#"["v1", "v2"]"#),
                        ("b", r#"["x"]"#)
                    ]),
                },
            ),
        ])
    });

    fn test_composition_of_directive_with_non_trivial_argument_strategies(
        test_case: &CompositionStrategyTestCase,
    ) {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: &format!(
                r#"
                extend schema @link(url: "https://specs.apollo.dev/{}/v0.1")

                type Query {{
                    t: T
                }}

                type T
                    @key(fields: "k")
                    @{}(value: {})
                {{
                    k: ID @{}(value: {})
                }}
                "#,
                test_case.name,
                test_case.name,
                test_case.arg_values_s1["t"],
                test_case.name,
                test_case.arg_values_s1["k"]
            ),
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: &format!(
                r#"
                extend schema @link(url: "https://specs.apollo.dev/{}/v0.1")

                type T
                    @key(fields: "k")
                    @{}(value: {})
                {{
                    k: ID @{}(value: {})
                    a: Int
                    b: String @{}(value: {})
                }}
                "#,
                test_case.name,
                test_case.name,
                test_case.arg_values_s2["t"],
                test_case.name,
                test_case.arg_values_s2["k"],
                test_case.name,
                test_case.arg_values_s2["b"]
            ),
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let result_sg = assert_composition_success(result);

        // Check expected hints
        let expected_hints = vec![
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: format!(
                    "Directive @{} is applied to \"T\" in multiple subgraphs with different arguments. Merging strategies used by arguments: {{ \"value\": {} }}",
                    test_case.name,
                    test_case.composition_strategy.name()
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: format!(
                    "Directive @{} is applied to \"T.k\" in multiple subgraphs with different arguments. Merging strategies used by arguments: {{ \"value\": {} }}",
                    test_case.name,
                    test_case.composition_strategy.name()
                ),
                locations: Vec::new(),
            },
        ];
        assert_hints_equal(result_sg.hints(), &expected_hints);

        // Check expected directive strings
        let schema = result_sg.schema().schema();
        assert_eq!(
            directive_strings_schema(&schema.schema_definition.directives, test_case.name),
            vec![format!(
                r#"@link(url: "https://specs.apollo.dev/{}/v0.1")"#,
                test_case.name
            )]
        );

        let t = schema.get_object("T").unwrap();
        assert_eq!(
            directive_strings_schema(&t.directives, test_case.name),
            [format!(
                r#"@{}(value: {})"#,
                test_case.name,
                test_case.result_values["t"]
            )]
        );
        assert_eq!(
            directive_strings_ast(&t.fields.get("k").unwrap().directives, test_case.name),
            [format!(
                r#"@{}(value: {})"#,
                test_case.name,
                test_case.result_values["k"]
            )]
        );
        assert_eq!(
            directive_strings_ast(&t.fields.get("b").unwrap().directives, test_case.name),
            [format!(
                r#"@{}(value: {})"#,
                test_case.name,
                test_case.result_values["b"]
            )]
        );
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_max() {
        let test_case = TEST_CASES.get("max").expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_min() {
        let test_case = TEST_CASES.get("min").expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_intersection() {
        let test_case = TEST_CASES.get("intersection").expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_union() {
        let test_case = TEST_CASES.get("union").expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_nullable_and() {
        let test_case = TEST_CASES.get("nullable_and").expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_nullable_max() {
        let test_case = TEST_CASES.get("nullable_max").expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn works_for_nullable_union() {
        let test_case = TEST_CASES
            .get("nullable_union")
            .expect("Test case not found");
        test_composition_of_directive_with_non_trivial_argument_strategies(test_case);
    }

    #[test]
    #[ignore = "Directive argument merge strategies not yet implemented"]
    fn errors_when_declaring_strategy_that_does_not_match_the_argument_type() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/foo/v0.1")

                type Query {
                    t: T
                }

                type T {
                    v: String @foo(value: "bar")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/foo/v0.1")

                type T {
                    v: String @foo(value: "bar")
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let errs = errors(&result);
        assert_eq!(
            errs.iter().map(|(_, message)| message).collect::<Vec<_>>(),
            [
                r#"Invalid composition strategy MAX for argument @foo(value:) of type String; MAX only supports type(s) Int!"#
            ]
        );
    }
}
