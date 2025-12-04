use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::utils::normalize_schema::normalize_schema;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;
use super::extract_subgraphs_from_supergraph_result;

fn compose_and_test_reversibility(subgraphs: &[ServiceDefinition<'_>]) {
    // Compose the original subgraphs
    let original_supergraph = compose_as_fed2_subgraphs(subgraphs)
        .expect("Subgraph schemas unexpectedly failed to compose.");

    // Extract subgraphs from the supergraph
    let extracted_subgraphs = extract_subgraphs_from_supergraph_result(&original_supergraph)
        .expect("Subgraph schemas unexpectedly unable to be extracted from supergraph schema.");

    // Verify all expected subgraphs were extracted
    for expected_subgraph_def in subgraphs {
        assert!(
            extracted_subgraphs
                .get(expected_subgraph_def.name)
                .is_some(),
            "Expected subgraph '{}' unexpectedly missing from extracted subgraphs.",
            expected_subgraph_def.name
        );
    }

    // Re-parse the extracted subgraphs
    let mut recomposed_subgraphs = Vec::new();
    for (name, extracted_subgraph) in extracted_subgraphs.into_iter() {
        let schema_string = extracted_subgraph.schema.schema().to_string();
        let subgraph = Subgraph::parse(&name, &extracted_subgraph.url, &schema_string)
            .expect("Extracted subgraph schema unexpectedly failed to re-parse.");
        recomposed_subgraphs.push(subgraph);
    }

    // Re-compose the extracted subgraphs using the compose function directly
    let recomposed_supergraph = compose(recomposed_subgraphs)
        .expect("Extracted subgraph schemas unexpectedly failed to re-compose.");

    // Verify that both supergraphs have the same structure by comparing their API schemas.
    // The supergraph schemas may differ in how they serialize directive arguments with default
    // values (e.g., "resolvable: true" vs omitted), but the API schemas should be identical.
    let original_api_schema = original_supergraph
        .to_api_schema(Default::default())
        .expect("Original supergraph unexpectedly failed to generate API schema.");
    let recomposed_api_schema = recomposed_supergraph
        .to_api_schema(Default::default())
        .expect("Recomposed supergraph unexpectedly failed to generate API schema.");

    let original_api_schema = normalize_schema(original_api_schema.schema().clone().into_inner());
    let recomposed_api_schema =
        normalize_schema(recomposed_api_schema.schema().clone().into_inner());

    assert_eq!(
        original_api_schema.to_string(),
        recomposed_api_schema.to_string(),
        "Original and recomposed API schemas don't match"
    );
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
