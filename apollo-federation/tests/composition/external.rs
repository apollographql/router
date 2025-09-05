use apollo_federation::error::CompositionError;
use apollo_federation::supergraph::Supergraph;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

fn error_messages<S>(result: &Result<Supergraph<S>, Vec<CompositionError>>) -> Vec<String> {
    match result {
        Ok(_) => panic!("Expected an error, but got a successful composition"),
        Err(err) => err.iter().map(|e| e.to_string()).collect(),
    }
}

fn assert_composition_success<S>(result: &Result<Supergraph<S>, Vec<CompositionError>>) {
    match result {
        Ok(_) => {}
        Err(err) => panic!("Expected successful composition, but got errors: {:?}", err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Compose directive manager validation not yet implemented."]
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
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [
                r#"Type of field "T.f" is incompatible across subgraphs (where marked @external): it has type "Int" in subgraph "subgraphB" but type "String" in subgraph "subgraphA""#
            ]
        );
    }

    #[test]
    #[ignore = "Compose directive manager validation not yet implemented."]
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
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [
                r#"Field "T.f" is missing argument "T.f(x:)" in some subgraphs where it is marked @external: argument "T.f(x:)" is declared in subgraph "subgraphB" but not in subgraph "subgraphA" (where "T.f" is @external)."#
            ]
        );
    }

    #[test]
    #[ignore = "Compose directive manager validation not yet implemented."]
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
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [
                r#"Type of argument "T.f(x:)" is incompatible across subgraphs (where "T.f" is marked @external): it has type "Int" in subgraph "subgraphB" but type "String" in subgraph "subgraphA""#
            ]
        );
    }

    #[test]
    #[ignore = "Compose directive manager validation not yet implemented."]
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
        assert_composition_success(&result);

        // Confirm the output schema is correct
        let supergraph_schema = r#"
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
        assert_eq!(&result.unwrap().schema().schema().to_string(), supergraph_schema);
    }
}
