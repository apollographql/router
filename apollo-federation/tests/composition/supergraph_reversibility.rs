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
    for expected_subgraph in subgraphs {
        let actual_subgraph = actual_subgraphs
            .get(expected_subgraph.name)
            .expect("Expected subgraph name unexpectedly missing from extracted subgraphs.");

        // PORT_NOTE: In the JS version of subgraph extraction, the extracted subgraphs are created
        // with their `@link` on the schema definition instead of a schema extension (there was no
        // strong reason for this, it's how the code was written). In the Rust version, `@link` is
        // instead on a schema extension, as per the code in `new_empty_fed_2_subgraph_schema()`, so
        // we don't have to work around that here as we did in the JS code.
        let expected_subgraph =
            Subgraph::parse(expected_subgraph.name, "", expected_subgraph.type_defs)
                .expect("Expected subgraph schema unexpectedly failed to parse.")
                .into_fed2_test_subgraph(
                    // PORT_NOTE: In the JS code, `asFed2SubgraphDocument()` would always add the
                    // latest federation spec, and the `includeAllImports` argument would just
                    // change which directives were imported (fed 2.4's directives or latest). Since
                    // the JS code's subgraph extraction uses latest fed spec but 2.4's imports, it
                    // made sense to leave `includeAllImports` as `false` in JS code.
                    //
                    // However, this JS subgraph-extraction behavior was due to it reusing code from
                    // composition (`setSchemaAsFed2Subgraph()`), which uses 2.4's imports to avoid
                    // directive name collisions with user-defined directives in subgraph schemas.
                    // For Rust code's subgraph extraction, it uses a separate function entirely
                    // called `new_empty_fed_2_subgraph_schema()`, which uses no imports for all
                    // federation directives.
                    //
                    // This means we need to specify `use_latest` as `true` here (which causes the
                    // Rust code to use latest federation spec), but specify `no_imports` as `true`
                    // to omit all imports.
                    true, true,
                )
                .expect("Expected subgraph schema unexpectedly failed to convert to Fed 2.");
        let actual_schema = actual_subgraph.schema.schema().clone().into_inner();
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
        let expected_schema = apollo_compiler::Schema::parse(
            expected_subgraph
                .expand_links()
                .expect("Expected subgraph schema unexpectedly failed @link expansion.")
                .schema_string(),
            "expanded.graphql",
        )
        .expect("Expanded expected subgraph schema unexpectedly failed to parse.");
        let actual_schema = normalize_schema(actual_schema);
        let expected_schema = normalize_schema(expected_schema);
        assert_eq!(actual_schema.to_string(), expected_schema.to_string())
    }
}

mod source_preserving_tests {
    use super::*;

    #[ignore = "until merge implementation completed"]
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

    #[ignore = "until merge implementation completed"]
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

    #[ignore = "until merge implementation completed"]
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
