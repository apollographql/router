use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;
use super::extract_subgraphs_from_supergraph_result;
use super::print_sdl; // TODO: Remove after rebasing onto merge commit of pull/8380

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
