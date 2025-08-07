// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'merging of type references'

use super::ServiceDefinition;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;

mod field_types {
    use super::*;
    use crate::merger::assert_api_schema_snapshot;
    use crate::merger::assert_error_contains;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_incompatible_types() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: String @shareable
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
        assert_error_contains(
            &result,
            "Type of field \"T.f\" is incompatible across subgraphs: it has type \"String\" in subgraph \"subgraphA\" but type \"Int\" in subgraph \"subgraphB\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_merging_list_type_with_non_list_version() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: String @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                  id: ID!
                  f: [String] @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Type of field \"T.f\" is incompatible across subgraphs: it has type \"String\" in subgraph \"subgraphA\" but type \"[String]\" in subgraph \"subgraphB\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn merges_nullable_and_non_nullable() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: String! @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                  id: ID!
                  f: String @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = assert_composition_success(&result);

        assert_api_schema_snapshot(supergraph);
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn merges_interface_subtypes() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                interface I {
                  a: Int
                }

                type A implements I @shareable {
                  a: Int
                  b: Int
                }

                type B implements I {
                  a: Int
                  c: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: I @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @shareable {
                  a: Int
                  b: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: A @shareable
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
    fn merges_union_subtypes() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                union U = A | B

                type A @shareable {
                  a: Int
                }

                type B {
                  b: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: U @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @shareable {
                  a: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: A @shareable
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
    fn merges_complex_subtypes() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                interface I {
                  a: Int
                }

                type A implements I @shareable {
                  a: Int
                  b: Int
                }

                type B implements I {
                  a: Int
                  c: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: I @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @shareable {
                  a: Int
                  b: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: A! @shareable
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
    fn merges_subtypes_within_lists() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                interface I {
                  a: Int
                }

                type A implements I @shareable {
                  a: Int
                  b: Int
                }

                type B implements I {
                  a: Int
                  c: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: [I] @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @shareable {
                  a: Int
                  b: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: [A!] @shareable
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
    fn merges_subtypes_within_non_nullable() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  T: T!
                }

                interface I {
                  a: Int
                }

                type A implements I @shareable {
                  a: Int
                  b: Int
                }

                type B implements I {
                  a: Int
                  c: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: I! @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @shareable {
                  a: Int
                  b: Int
                }

                type T @key(fields: "id") {
                  id: ID!
                  f: A! @shareable
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
    fn errors_on_incompatible_input_field_types() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  q: String
                }

                input T {
                  f: String
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                input T {
                  f: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Type of field \"T.f\" is incompatible across subgraphs: it has type \"String\" in subgraph \"subgraphA\" but type \"Int\" in subgraph \"subgraphB\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_incompatible_input_field_default_values() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                  q: String
                }

                input T {
                  f: Int = 0
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                input T {
                  f: Int = 1
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Input field \"T.f\" has incompatible default values across subgraphs: it has default value 0 in subgraph \"subgraphA\" but default value 1 in subgraph \"subgraphB\"",
        );
    }
}

mod arguments {
    use super::*;
    use crate::merger::assert_api_schema_snapshot;
    use crate::merger::assert_error_contains;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_incompatible_types() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                type T @key(fields: "id") {
                    id: ID!
                    f(x: Int): Int @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: String): Int @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Type of argument \"T.f(x:)\" is incompatible across subgraphs: it has type \"Int\" in subgraph \"subgraphA\" but type \"String\" in subgraph \"subgraphB\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_incompatible_argument_default() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                type T @key(fields: "id") {
                    id: ID!
                    f(x: Int = 0): String @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: Int = 1): String @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Argument \"T.f(x:)\" has incompatible default values across subgraphs: it has default value 0 in subgraph \"subgraphA\" but default value 1 in subgraph \"subgraphB\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_incompatible_argument_default_in_external_declaration() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                interface I {
                    f(x: Int): String
                }

                type T implements I @key(fields: "id") {
                    id: ID!
                    f(x: Int): String @external
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: Int = 1): String
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Argument \"T.f(x:)\" has incompatible defaults across subgraphs (where \"T.f\" is marked @external): it has default value 1 in subgraph \"subgraphB\" but no default value in subgraph \"subgraphA\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_on_merging_a_list_type_with_a_non_list_version() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                type T @key(fields: "id") {
                    id: ID!
                    f(x: String): String @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: [String]): String @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        assert_error_contains(
            &result,
            "Type of argument \"T.f(x:)\" is incompatible across subgraphs: it has type \"String\" in subgraph \"subgraphA\" but type \"[String]\" in subgraph \"subgraphB\"",
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn merges_nullable_and_non_nullable() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                type T @key(fields: "id") {
                    id: ID!
                    f(x: String): String @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: String!): String @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = assert_composition_success(&result);

        // We expect `f(x:)` to be non-nullable.
        assert_api_schema_snapshot(supergraph);
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn merges_subtypes_within_lists() {
        // This example merge types that differs both on interface subtyping
        // and on nullability
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    T: T!
                }

                type T @key(fields: "id") {
                    id: ID!
                    f(x: [Int]): Int @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: ID!
                    f(x: [Int!]): Int @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = assert_composition_success(&result);

        // We expect `f` to be `[Int!]` as that is the merged result.
        assert_api_schema_snapshot(supergraph);
    }
}
