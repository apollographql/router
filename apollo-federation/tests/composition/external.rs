use apollo_federation::error::CompositionError;
use apollo_federation::supergraph::Supergraph;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

fn errors<S>(result: &Result<Supergraph<S>, Vec<CompositionError>>) -> Vec<(String, String)> {
    match result {
        Ok(_) => panic!("Expected an error, but got a successful composition"),
        Err(err) => err
            .iter()
            .map(|e| (e.code().definition().code().to_string(), e.to_string()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Merger::merge() sub-functions not fully implemented."]
    fn errors_on_incompatible_types_with_external() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T! @provides(fields: "f")
                }

                type T @key(fields: "id") {
                    id: ID!
                    f: String @external
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f: Int @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let errors = errors(&result);
        itertools::assert_equal(
            errors,
            [(
                "EXTERNAL_TYPE_MISMATCH".to_owned(),
                r#"Type of field "T.f" is incompatible across subgraphs (where marked @external): it has type "Int" in subgraph "subgraphB" but type "String" in subgraph "subgraphA""#.to_owned()
            )]
        );
    }

    #[test]
    #[ignore = "Merger::merge() sub-functions not fully implemented."]
    fn errors_on_missing_arguments_to_external_declaration() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T! @provides(fields: "f")
                }

                type T @key(fields: "id") {
                    id: ID!
                    f: String @external
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: Int): String @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let errors = errors(&result);
        itertools::assert_equal(
            errors,
            [(
                "EXTERNAL_ARGUMENT_MISSING".to_owned(),
                r#"Field "T.f" is missing argument "T.f(x:)" in some subgraphs where it is marked @external: argument "T.f(x:)" is declared in subgraph "subgraphB" but not in subgraph "subgraphA" (where "T.f" is @external)."#.to_owned()
            )]
        );
    }

    #[test]
    #[ignore = "Merger::merge() sub-functions not fully implemented."]
    fn errors_on_incompatible_argument_types_in_external_declaration() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                interface I {
                    f(x: String): String
                }

                type T implements I @key(fields: "id") {
                    id: ID!
                    f(x: String): String @external
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: Int): String
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let errors = errors(&result);
        itertools::assert_equal(
            errors,
            [(
                "EXTERNAL_ARGUMENT_TYPE_MISMATCH".to_owned(),
                r#"Type of argument "T.f(x:)" is incompatible across subgraphs (where "T.f" is marked @external): it has type "Int" in subgraph "subgraphB" but type "String" in subgraph "subgraphA""#.to_owned()
            )]
        );
    }

    #[test]
    #[ignore = "Merger::merge() sub-functions not fully implemented."]
    fn external_marked_on_type() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                type T @key(fields: "id") {
                    id: ID!
                    x: X @external
                    y: Int @requires(fields: "x { a b c d }")
                }

                type X @external {
                    a: Int
                    b: Int
                    c: Int
                    d: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    x: X
                }

                type X {
                    a: Int
                    b: Int
                    c: Int
                    d: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let result_supergraph = result.expect("Expect successful composition");

        // Confirm the output schema is correct
        let expected_supergraph_schema = r#"
            type Query {
                T: T!
            }

            type T {
                id: ID!
                x: X
                y: Int
            }

            type X {
                a: Int
                b: Int
                c: Int
                d: Int
            }
        "#;
        assert_eq!(
            result_supergraph
                .to_api_schema(Default::default())
                .expect("api schema")
                .schema()
                .to_string(),
            expected_supergraph_schema
        );
    }
}
