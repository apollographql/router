use std::fs;

use apollo_compiler::coord;
use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;
use super::extract_subgraphs_from_supergraph_result;

const DATA_DUMP_DIR: &str = "/Users/dkuc/Development/federation-performance-harness/data-dump";

const UUIDS: &[&str] = &[
    // Group 3.8: RS-only errors (DIRECTIVE_COMPOSITION_ERROR)
    "042dd710-0519-49ba-ae14-df7ec0e1cbbd",
    "054a68d1-8291-455a-9438-42ab4aa72136",
    "07de587d-8125-4240-ba2e-c328f612d83a",
    // Group 3.8: RS-only errors (INVALID_GRAPHQL)
    "01113882-4687-447f-989b-4eddf97ab647",
    "01a4a489-f18b-4318-bd21-4a380f69349f",
    "01c782fd-6bf8-4597-b046-1c82bda460cd",
    // Group 3.8: RS-only errors (INTERNAL)
    "010a7788-8295-4e07-90f9-90b1d4f09cd8",
    "02a68cc7-7403-4fa5-8216-091d374046b1",
    "02d768d4-d47a-4f98-be40-89258a59d5d2",
    // Group 3.8: RS-only errors (UNKNOWN_ERROR_CODE)
    "006e2bab-3381-41a1-bd90-5ebf3f68e7e0",
    "01603f3b-f1f1-4090-9644-35ea6deb3253",
    "01a4b19e-4543-4569-8c55-0c47292ad4e1",
    // Group 3.8: RS-only errors (SATISFIABILITY_ERROR)
    "03428beb-4a30-4982-ad00-584f9643f44c",
    "04639474-9aa2-4d7c-88d2-cfec85871d96",
    "04be760f-2ccd-4c48-96ca-154f80e0b2a2",
    // Group 3.9: RS file missing (crash/failure)
    "007c5530-c001-41fa-b5ef-db232308ebb7",
    "009a7caf-57d8-438d-b595-5836d75d4ee4",
    "00c1b18b-573f-4a93-80bc-7b2192cf05f2",
];

fn load_subgraphs(uuid: &str) -> Vec<Subgraph<apollo_federation::subgraph::typestate::Initial>> {
    let subgraphs_dir = format!("{DATA_DUMP_DIR}/{uuid}/subgraphs");
    let mut subgraphs = Vec::new();
    for entry in
        fs::read_dir(&subgraphs_dir).unwrap_or_else(|e| panic!("cannot read {subgraphs_dir}: {e}"))
    {
        let entry = entry.expect("valid dir entry");
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "graphql") {
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            let sdl = fs::read_to_string(&path).expect("sdl exists");
            let subgraph = Subgraph::parse(&name, &format!("http://{name}"), &sdl)
                .unwrap_or_else(|e| panic!("failed to parse {name}: {e}"));
            subgraphs.push(subgraph);
        }
    }
    subgraphs
}

#[test]
fn integration_test_for_debugging_issues() {
    let mut passed = Vec::new();
    let mut failed = Vec::new();

    let out_dir = "/tmp/rust-supergraphs";
    fs::create_dir_all(out_dir).unwrap();

    let mut seen = std::collections::HashSet::new();
    for uuid in UUIDS {
        if !seen.insert(*uuid) {
            continue;
        }
        let subgraphs = load_subgraphs(uuid);
        match compose(subgraphs, Default::default()) {
            Ok(supergraph) => {
                passed.push(*uuid);
                let sdl = supergraph.schema().schema().to_string();
                fs::write(format!("{out_dir}/{uuid}.graphql"), sdl).unwrap();
            }
            Err(failure) => {
                let msgs: Vec<String> = failure.errors.iter().map(|e| e.to_string()).collect();
                eprintln!("FAIL {uuid}:\n  {}", msgs.join("\n  "));
                failed.push(*uuid);
            }
        }
    }

    eprintln!("\n=== RESULTS ===");
    eprintln!("Passed: {}/{}", passed.len(), UUIDS.len());
    eprintln!("Failed: {}/{}", failed.len(), UUIDS.len());
    for uuid in &failed {
        eprintln!("  FAIL: {uuid}");
    }

    assert!(
        failed.is_empty(),
        "{} out of {} UUIDs failed composition: {:?}",
        failed.len(),
        UUIDS.len(),
        failed
    );
}

#[test]
fn generates_a_valid_supergraph() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "k") {
          k: ID
        }

        type S {
          x: Int
        }

        union U = S | T
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type T @key(fields: "k") {
          k: ID
          a: Int
          b: String
        }

        enum E {
          V1
          V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    assert_snapshot!(supergraph.schema().schema());
    assert_snapshot!(api_schema.schema());
}

/// Ensures that when a type T implements an interface I in both a base definition (Subgraph1)
/// and an extension (Subgraph2), the supergraph SDL emits "type T implements I" on the
/// main type definition line rather than splitting into "type T" + "extend type T implements I".
#[test]
fn implements_on_type_definition_not_extend_type() {
    let subgraph_a = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            interface I {
              id: ID!
            }

            type T implements I @key(fields: "id") {
              id: ID!
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            interface I {
              id: ID!
            }

            extend type T implements I @key(fields: "id") {
              id: ID!
              note: String
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]).expect("composition should succeed");
    assert_snapshot!(supergraph.schema().schema());
}

#[test]
fn preserves_descriptions() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        "The foo directive description"
        directive @foo(url: String) on FIELD

        "A cool schema"
        schema {
          query: Query
        }

        """
        Available queries
        Not much yet
        """
        type Query {
          "Returns tea"
          t(
            "An argument that is very important"
            x: String!
          ): String
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        "The foo directive description"
        directive @foo(url: String) on FIELD

        "An enum"
        enum E {
          "The A value"
          A
          "The B value"
          B
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(api_schema.schema());
}

#[test]
fn no_hint_raised_when_merging_empty_description() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        schema {
          query: Query
        }

        ""
        type T {
          a: String @shareable
        }

        type Query {
          "Returns tea"
          t(
            "An argument that is very important"
            x: String!
          ): T
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        "Type T"
        type T {
          a: String @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let supergraph = result.expect("Expected composition to succeed");

    // Verify that no hints are raised when merging empty description with non-empty description
    assert_eq!(
        supergraph.hints().len(),
        0,
        "Expected no hints but got: {:?}",
        supergraph.hints()
    );
}

#[test]
fn include_types_from_different_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          products: [Product!]
        }

        type Product {
          sku: String!
          name: String!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User {
          name: String
          email: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(api_schema.schema());

    // Validate extracted subgraphs contain proper federation directives
    let extracted_subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
        .expect("Expected subgraph extraction to succeed");

    let subgraph_a_extracted = extracted_subgraphs
        .get("subgraphA")
        .expect("Expected subgraphA to be present in extracted subgraphs");
    assert_snapshot!(subgraph_a_extracted.schema.schema());

    let subgraph_b_extracted = extracted_subgraphs
        .get("subgraphB")
        .expect("Expected subgraphB to be present in extracted subgraphs");
    assert_snapshot!(subgraph_b_extracted.schema.schema());
}

#[test]
fn doesnt_leave_federation_directives_in_the_final_schema() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          products: [Product!] @provides(fields: "name")
        }

        type Product @key(fields: "sku") {
          sku: String!
          name: String! @external
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Product @key(fields: "sku") {
          sku: String!
          name: String! @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(api_schema.schema());

    // Validate that federation directives (@provides, @key, @external, @shareable)
    // are properly rebuilt in the extracted subgraphs
    let extracted_subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
        .expect("Expected subgraph extraction to succeed");

    let subgraph_a_extracted = extracted_subgraphs
        .get("subgraphA")
        .expect("Expected subgraphA to be present in extracted subgraphs");
    assert_snapshot!(subgraph_a_extracted.schema.schema());

    let subgraph_b_extracted = extracted_subgraphs
        .get("subgraphB")
        .expect("Expected subgraphB to be present in extracted subgraphs");
    assert_snapshot!(subgraph_b_extracted.schema.schema());
}

#[test]
fn merges_default_arguments_when_they_are_arrays() {
    let subgraph_a = ServiceDefinition {
        name: "subgraph-a",
        type_defs: r#"
        type Query {
          a: A @shareable
        }

        type A @key(fields: "id") {
          id: ID
          get(ids: [ID] = []): [B] @external
          req: Int @requires(fields: "get { __typename }")
        }

        type B @key(fields: "id", resolvable: false) {
          id: ID
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraph-b",
        type_defs: r#"
        type Query {
          a: A @shareable
        }

        type A @key(fields: "id") {
          id: ID
          get(ids: [ID] = []): [B]
        }

        type B @key(fields: "id") {
          id: ID
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph = result.expect("Expected composition to succeed");
}

#[test]
fn removes_redundant_join_field_directives() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          product(id: ID!): Product
        }

        type Product @key(fields: "id") {
          id: ID!
          name: String! @shareable
          price: Float! @shareable
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Product @key(fields: "id") {
          id: ID!
          name: String! @shareable
          price: Float! @shareable
          description: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let supergraph = result.expect("Expected composition to succeed");
    let schema = supergraph.schema().schema();

    // Fields in both subgraphs should not have @join__field
    for field_coord in [
        coord!(Product.id),
        coord!(Product.name),
        coord!(Product.price),
    ] {
        let field = field_coord.lookup_field(schema).expect("Field exists");
        let has_join_field = field.directives.iter().any(|d| d.name == "join__field");
        assert!(
            !has_join_field,
            "Field {} should not have @join__field directives",
            field_coord
        );
    }

    // Field only in one subgraph should have @join__field
    let description_field = coord!(Product.description)
        .lookup_field(schema)
        .expect("Field exists");
    let has_join_field = description_field
        .directives
        .iter()
        .any(|d| d.name == "join__field");
    assert!(
        has_join_field,
        "Field Product.description should have @join__field directive"
    );
}
