use apollo_federation::Supergraph;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::utils::normalize_schema::normalize_schema;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

fn compose_and_test_reversibility(subgraphs: &[ServiceDefinition<'_>]) {
    let result = compose_as_fed2_subgraphs(subgraphs)
        .expect("Subgraph schemas unexpectedly failed to compose.");

    let actual_subgraphs = Supergraph::new(&result.schema().schema().to_string())
        .expect("Supergraph schema unexpectedly failed to validate.")
        .extract_subgraphs()
        .expect("Subgraph schemas unexpectedly unable to be extracted from supergraph schema.");
    for expected in subgraphs {
        let actual_subgraph = actual_subgraphs
            .get(expected.name)
            .expect("Expected subgraph name unexpectedly missing from extracted subgraphs.");

        // PORT_NOTE: In the JS code, only `asFed2SubgraphDocument()` is called for the expected
        // schema, so link/federation spec definitions are not expanded, nor are federation
        // operation fields/types added (`Query._entities`, `_Entity`, etc.). This ends up being
        // okay for the JS code since `Subgraph`s have a `toString()` implementation that will omit
        // federation spec and link spec directive/type definitions, along with federation operation
        // fields/types.
        //
        // However, this is generally bad for testing, since we could be omitting schema elements
        // with differences that indicate bugs. So in the Rust code, we instead use `expand_links()`
        // since it both expands link/federation spec definitions and adds federation operation
        // fields/types.
        let expected_subgraph = Subgraph::parse(expected.name, "", expected.type_defs)
            .expect("Expected subgraph schema unexpectedly failed to parse.")
            .into_fed2_test_subgraph(false)
            .expect("Expected subgraph schema unexpectedly failed to convert to Fed 2.")
            .expand_links()
            .expect("Expected subgraph schema unexpectedly failed to expand")
            .assume_validated();

        let actual_schema = actual_subgraph.schema.schema().clone().into_inner();
        let actual_schema = normalize_schema(actual_schema);

        let expected_schema = expected_subgraph.schema().clone().into_inner();
        let expected_schema = normalize_schema(expected_schema);
        assert_eq!(actual_schema.to_string(), expected_schema.to_string())
    }
}

mod source_preserving_tests {
    use super::*;

    #[test]
    fn preserves_the_source_of_union_members() {
        let subgraph_s1 = ServiceDefinition {
            name: "S1",
            type_defs: r#"
                type Query {
                    uFromS1: U
                }

                union U = A | B

                type A {
                    a: Int
                }

                type B {
                    b: Int @shareable
                }
            "#,
        };

        let subgraph_s2 = ServiceDefinition {
            name: "S2",
            type_defs: r#"
                type Query {
                    uFromS2: U
                }

                union U = B | C

                type B {
                    b: Int @shareable
                }

                type C {
                    c: Int
                }
            "#,
        };

        compose_and_test_reversibility(&[subgraph_s1, subgraph_s2]);
    }

    #[test]
    fn preserves_the_source_of_enum_members() {
        let subgraph_s1 = ServiceDefinition {
            name: "S1",
            type_defs: r#"
                type Query {
                    eFromS1: E
                }

                enum E {
                    A,
                    B
                }
            "#,
        };

        let subgraph_s2 = ServiceDefinition {
            name: "S2",
            type_defs: r#"
                type Query {
                    eFromS2: E
                }

                enum E {
                    B,
                    C
                }
            "#,
        };

        compose_and_test_reversibility(&[subgraph_s1, subgraph_s2]);
    }
}

mod interface_object_tests {
    use super::*;

    #[test]
    fn correctly_extract_external_fields_of_concrete_type_only_provided_by_an_interface_object() {
        let subgraph_s1 = ServiceDefinition {
            name: "S1",
            type_defs: r#"
                type Query {
                    iFromS1: I
                }

                interface I @key(fields: "id") {
                    id: ID!
                    x: Int
                }

                type T implements I @key(fields: "id") {
                    id: ID!
                    x: Int @external
                    y: Int @requires(fields: "x")
                }
            "#,
        };

        let subgraph_s2 = ServiceDefinition {
            name: "S2",
            type_defs: r#"
                type Query {
                    iFromS2: I
                }

                type I @interfaceObject @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        compose_and_test_reversibility(&[subgraph_s1, subgraph_s2]);
    }
}
