use apollo_compiler::coord;
use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;
use super::extract_subgraphs_from_supergraph_result;
use super::print_sdl;

// =============================================================================
// FIELD TYPES - Tests for field type compatibility during composition
// =============================================================================

#[test]
fn field_types_errors_on_incompatible_types() {
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
    assert_composition_errors(
        &result,
        &[(
            "FIELD_TYPE_MISMATCH",
            r#"Type of field "T.f" is incompatible across subgraphs: it has type "String" in subgraph "subgraphA" but type "Int" in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn field_types_errors_on_merging_list_with_non_list() {
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
    assert_composition_errors(
        &result,
        &[(
            "FIELD_TYPE_MISMATCH",
            r#"Type of field "T.f" is incompatible across subgraphs: it has type "String" in subgraph "subgraphA" but type "[String]" in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn field_types_merges_nullable_and_non_nullable() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // We expect `f` to be nullable (String, not String!)
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      T: T!
    }

    type T {
      id: ID!
      f: String
    }
    "###);
}

#[test]
fn field_types_merges_interface_subtypes() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // We expect `f` to be `I` as that is the supertype between itself and `A`
    assert_snapshot!(print_sdl(api_schema.schema()));

    // Validate that field types are properly preserved in extracted subgraphs
    let extracted_subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
        .expect("Expected subgraph extraction to succeed");

    // Ensure we properly extract the original field types in each subgraph
    let subgraph_a_extracted = extracted_subgraphs
        .get("subgraphA")
        .expect("Expected subgraphA to be present in extracted subgraphs");
    let f = coord!(T.f)
        .lookup_field(subgraph_a_extracted.schema.schema())
        .expect("Expected T.f to be present in subgraphA");
    assert_eq!(f.ty.to_string(), "I");

    let subgraph_b_extracted = extracted_subgraphs
        .get("subgraphB")
        .expect("Expected subgraphB to be present in extracted subgraphs");
    let f = coord!(T.f)
        .lookup_field(subgraph_b_extracted.schema.schema())
        .expect("Expected T.f to be present in subgraphB");
    assert_eq!(f.ty.to_string(), "A");
}

#[test]
fn field_types_merges_union_subtypes() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // We expect `f` to be `U` as that is the supertype between itself and `A`
    assert_snapshot!(print_sdl(api_schema.schema()));

    // Validate that field types are properly preserved in extracted subgraphs
    let extracted_subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
        .expect("Expected subgraph extraction to succeed");

    // Ensure we properly extract the original field types in each subgraph
    let subgraph_a_extracted = extracted_subgraphs
        .get("subgraphA")
        .expect("Expected subgraphA to be present in extracted subgraphs");
    let f = coord!(T.f)
        .lookup_field(subgraph_a_extracted.schema.schema())
        .expect("Expected T.f to be present in subgraphA");
    assert_eq!(f.ty.to_string(), "U");

    let subgraph_b_extracted = extracted_subgraphs
        .get("subgraphB")
        .expect("Expected subgraphB to be present in extracted subgraphs");
    let f = coord!(T.f)
        .lookup_field(subgraph_b_extracted.schema.schema())
        .expect("Expected T.f to be present in subgraphB");
    assert_eq!(f.ty.to_string(), "A");
}

#[test]
fn field_types_merges_complex_subtypes() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Field should merge to the common supertype
    assert_snapshot!(print_sdl(api_schema.schema()));
}

#[test]
fn field_types_merges_subtypes_within_lists() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should merge list element types while preserving list structure
    assert_snapshot!(print_sdl(api_schema.schema()));
}

#[test]
fn field_types_merges_subtypes_within_non_nullable() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should merge to nullable interface type
    assert_snapshot!(print_sdl(api_schema.schema()));
}

#[test]
fn field_types_errors_on_incompatible_input_field_types_first() {
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
    assert_composition_errors(
        &result,
        &[(
            "FIELD_TYPE_MISMATCH",
            r#"Type of field "T.f" is incompatible across subgraphs: it has type "String" in subgraph "subgraphA" but type "Int" in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn field_types_errors_on_incompatible_input_field_types_second() {
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
    assert_composition_errors(
        &result,
        &[(
            "INPUT_FIELD_DEFAULT_MISMATCH",
            r#"Input field "T.f" has incompatible default values across subgraphs: it has default value 0 in subgraph "subgraphA" but default value 1 in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn composes_covariant_containers() {
    // Test that [U]! (non-null list) is compatible with [U] (nullable list)
    // when merging fields across subgraphs
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "id") {
          id: ID!
          listField: [U] @shareable
          otherField: V @shareable
        }

        type U @shareable {
          a: String
        }

        type V @shareable {
          b: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type T @key(fields: "id") {
          id: ID!
          listField: [U]! @shareable
          otherField: V! @shareable
        }

        type U @shareable {
          a: String
        }

        type V @shareable {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // The merged type should use the more general (nullable) type for output positions
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      t: T
    }

    type T {
      id: ID!
      listField: [U]
      otherField: V
    }

    type U {
      a: String
    }

    type V {
      b: Int
    }
    "###);
}

// =============================================================================
// ARGUMENTS - Tests for argument type compatibility during composition
// =============================================================================

#[test]
fn arguments_errors_on_incompatible_types() {
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
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_TYPE_MISMATCH",
            r#"Type of argument "T.f(x:)" is incompatible across subgraphs: it has type "Int" in subgraph "subgraphA" but type "String" in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn arguments_errors_on_incompatible_argument_default() {
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
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_DEFAULT_MISMATCH",
            r#"Argument "T.f(x:)" has incompatible default values across subgraphs: it has default value 0 in subgraph "subgraphA" but default value 1 in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn arguments_errors_on_incompatible_argument_default_in_external_declaration() {
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
    assert_composition_errors(
        &result,
        &[(
            "EXTERNAL_ARGUMENT_DEFAULT_MISMATCH",
            r#"Argument "T.f(x:)" has incompatible defaults across subgraphs (where "T.f" is marked @external): it has default value 1 in subgraph "subgraphB" but no default value in subgraph "subgraphA""#,
        )],
    );
}

#[test]
fn arguments_errors_on_merging_list_with_non_list() {
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
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_TYPE_MISMATCH",
            r#"Type of argument "T.f(x:)" is incompatible across subgraphs: it has type "String" in subgraph "subgraphA" but type "[String]" in subgraph "subgraphB""#,
        )],
    );
}

#[test]
fn arguments_merges_nullable_and_non_nullable() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Argument should merge to non-nullable (String!)
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      T: T!
    }

    type T {
      id: ID!
      f(x: String!): String
    }
    "###);
}

#[test]
fn arguments_merges_subtypes_within_lists() {
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
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // We expect the merged argument to be [Int!]
    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    type Query {
      T: T!
    }

    type T {
      id: ID!
      f(x: [Int!]): Int
    }
    "###);
}

// =============================================================================
// EXTENSION_WITH_NO_BASE validation
// =============================================================================

#[test]
fn report_extension_with_no_base() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            directive @dir on SCALAR

            extend scalar MyScalar @dir

            type Query {
                test: MyScalar
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    assert_composition_errors(
        &result,
        &[(
            "EXTENSION_WITH_NO_BASE",
            r#"[subgraphA] Type "MyScalar" is an extension type, but there is no type definition for "MyScalar" in any subgraph."#,
        )],
    );
}

#[test]
fn handle_extension_with_empty_base() {
    // This test used to panic.
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            directive @dir on SCALAR

            scalar MyScalar # Note: empty base type definition

            extend scalar MyScalar @dir

            type Query {
                test: MyScalar
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    assert_snapshot!(print_sdl(api_schema.schema()), @r###"
    scalar MyScalar

    type Query {
      test: MyScalar
    }
    "###);
}
