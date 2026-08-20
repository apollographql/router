use insta::assert_snapshot;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose;
use super::compose_as_fed2_subgraphs;

#[cfg(test)]
mod tests {
    use apollo_federation::subgraph::typestate::Subgraph;

    use super::*;

    #[test]
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
        assert_composition_errors(
            &result,
            &[(
                "EXTERNAL_TYPE_MISMATCH",
                r#"Type of field "T.f" is incompatible across subgraphs (where marked @external): it has type "Int" in subgraph "subgraphB" but type "String" in subgraph "subgraphA""#,
            )],
        );
    }

    #[test]
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
        assert_composition_errors(
            &result,
            &[(
                "EXTERNAL_ARGUMENT_MISSING",
                r#"Field "T.f" is missing argument "T.f(x:)" in some subgraphs where it is marked @external: argument "T.f(x:)" is declared in subgraph "subgraphB" but not in subgraph "subgraphA" (where "T.f" is @external)."#,
            )],
        );
    }

    #[test]
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
        assert_composition_errors(
            &result,
            &[(
                "EXTERNAL_ARGUMENT_TYPE_MISMATCH",
                r#"Type of argument "T.f(x:)" is incompatible across subgraphs (where "T.f" is marked @external): it has type "Int" in subgraph "subgraphB" but type "String" in subgraph "subgraphA""#,
            )],
        );
    }

    #[test]
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

        let supergraph = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b])
            .expect("Expect successful composition");
        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("api schema")
            .schema()
            .to_string();

        assert_snapshot!(api_schema, @r###"
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
        "###);
    }

    /// Regression test: when a Fed v2 subgraph uses type-level @external
    /// (e.g., `type T @key(fields: "id") @external { id: ID! }`), the composed
    /// supergraph must emit `@join__field(graph: ..., external: true)` for the
    /// key field — not drop all @join__field directives.
    #[test]
    fn type_level_external_preserves_join_field_with_external_arg() {
        // s1: owns T, provides field a
        let s1 = Subgraph::parse(
            "s1",
            "http://s1",
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@external", "@requires"])

                directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
                directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
                directive @external on OBJECT | FIELD_DEFINITION
                directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

                enum link__Purpose { SECURITY EXECUTION }
                scalar link__Import
                scalar federation__FieldSet

                type Query {
                    ts: [T!]!
                }

                type T @key(fields: "id") {
                    id: ID!
                    a: String!
                }

                type S @key(fields: "t { id }") {
                    t: T!
                    x: ID
                }
            "#,
        )
        .unwrap();

        // s2: also owns T, provides field b
        let s2 = Subgraph::parse(
            "s2",
            "http://s2",
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@external"])

                directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
                directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
                directive @external on OBJECT | FIELD_DEFINITION

                enum link__Purpose { SECURITY EXECUTION }
                scalar link__Import
                scalar federation__FieldSet

                type T @key(fields: "id") {
                    id: ID!
                    b: String!
                }
            "#,
        )
        .unwrap();

        // s3: references T as an external stub (type-level @external).
        // Extends S from s1 and uses @requires on x.
        let s3 = Subgraph::parse(
            "s3",
            "http://s3",
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@external", "@extends", "@requires"])

                directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
                directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
                directive @external on OBJECT | FIELD_DEFINITION
                directive @extends on OBJECT | INTERFACE
                directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

                enum link__Purpose { SECURITY EXECUTION }
                scalar link__Import
                scalar federation__FieldSet

                type T @key(fields: "id") @external {
                    id: ID!
                }

                type S @key(fields: "t { id }") @extends {
                    t: T! @external
                    x: ID @external
                    y: [String!] @requires(fields: "x")
                }
            "#,
        )
        .unwrap();

        let supergraph = compose(vec![s1, s2, s3]).expect("composition should succeed");
        assert_snapshot!(supergraph.schema().schema().to_string());
    }
}
