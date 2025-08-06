// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'composition'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;

#[ignore = "until merge implementation completed"]
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
    let supergraph = assert_composition_success(&result);

    insta::assert_snapshot!(supergraph.schema().schema());
    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn respects_given_compose_options() {
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
    let supergraph = assert_composition_success(&result);

    insta::assert_snapshot!(supergraph.schema().schema());
    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
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
    let _ = assert_composition_success(&result);

    assert_eq!(result.unwrap().hints().len(), 0);
}

#[ignore = "until merge implementation completed"]
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
    let supergraph = assert_composition_success(&result);

    insta::assert_snapshot!(supergraph.schema().schema());
    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn doesnt_leave_federation_directives_in_final_schema() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_default_arguments_when_they_are_arrays() {
    let subgraph_a: ServiceDefinition<'_> = ServiceDefinition {
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
    let _ = assert_composition_success(&result);
}

#[ignore = "until merge implementation completed"]
#[test]
fn works_with_normal_graphql_type_extension_when_definition_is_empty() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          foo: Foo
        }

        type Foo

        extend type Foo {
          bar: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    let _ = assert_composition_success(&result);
}
