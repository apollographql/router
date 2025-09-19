use insta::assert_snapshot;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;
use super::extract_subgraphs_from_supergraph_result;
use super::print_sdl;

// =============================================================================
// FIELD TYPES - Tests for field type compatibility during composition
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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
#[ignore = "until merge implementation completed"]
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

    let subgraph_a_extracted = extracted_subgraphs
        .get("subgraphA")
        .expect("Expected subgraphA to be present in extracted subgraphs");
    assert_snapshot!(print_sdl(subgraph_a_extracted.schema.schema()));

    let subgraph_b_extracted = extracted_subgraphs
        .get("subgraphB")
        .expect("Expected subgraphB to be present in extracted subgraphs");
    assert_snapshot!(print_sdl(subgraph_b_extracted.schema.schema()));
}

#[test]
#[ignore = "until merge implementation completed"]
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

    let subgraph_a_extracted = extracted_subgraphs
        .get("subgraphA")
        .expect("Expected subgraphA to be present in extracted subgraphs");
    assert_snapshot!(print_sdl(subgraph_a_extracted.schema.schema()));

    let subgraph_b_extracted = extracted_subgraphs
        .get("subgraphB")
        .expect("Expected subgraphB to be present in extracted subgraphs");
    assert_snapshot!(print_sdl(subgraph_b_extracted.schema.schema()));
}

#[test]
#[ignore = "until merge implementation completed"]
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

        union U = A | B

        type T @key(fields: "id") {
          id: ID!
          f: U @shareable
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface I {
          a: Int
        }

        type A implements I @shareable {
          a: Int
          b: Int
        }

        type T @key(fields: "id") {
          id: ID!
          f: I @shareable
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
#[ignore = "until merge implementation completed"]
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

        union U = A | B

        type T @key(fields: "id") {
          id: ID!
          f: [U!]! @shareable
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface I {
          a: Int
        }

        type A implements I @shareable {
          a: Int
          b: Int
        }

        type T @key(fields: "id") {
          id: ID!
          f: [I]! @shareable
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
#[ignore = "until merge implementation completed"]
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

        union U = A | B

        type T @key(fields: "id") {
          id: ID!
          f: U! @shareable
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface I {
          a: Int
        }

        type A implements I @shareable {
          a: Int
          b: Int
        }

        type T @key(fields: "id") {
          id: ID!
          f: I @shareable
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
#[ignore = "until merge implementation completed"]
fn field_types_errors_on_incompatible_input_field_types_first() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(input: MyInput): String
        }

        input MyInput {
          field: String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        input MyInput {
          field: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "FIELD_TYPE_MISMATCH",
            r#"Type of field "MyInput.field" is incompatible across subgraphs"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn field_types_errors_on_incompatible_input_field_types_second() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(input: MyInput): String
        }

        input MyInput {
          field: [String]
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        input MyInput {
          field: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "FIELD_TYPE_MISMATCH",
            r#"Type of field "MyInput.field" is incompatible across subgraphs"#,
        )],
    );
}

// =============================================================================
// ARGUMENTS - Tests for argument type compatibility during composition
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn arguments_errors_on_incompatible_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          field(arg: String): String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          field(arg: Int): String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_TYPE_MISMATCH",
            r#"Type of argument "Query.field(arg:)" is incompatible across subgraphs"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn arguments_errors_on_incompatible_argument_default() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          field(arg: String = "a"): String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          field(arg: String = "b"): String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_DEFAULT_MISMATCH",
            r#"Default value of argument "Query.field(arg:)" is incompatible across subgraphs"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn arguments_errors_on_incompatible_argument_default_in_external_declaration() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "id") {
          id: ID!
          field(arg: String = "a"): String @external
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type T @key(fields: "id") {
          id: ID!
          field(arg: String = "b"): String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_DEFAULT_MISMATCH",
            r#"Default value of argument "T.field(arg:)" is incompatible across subgraphs"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn arguments_errors_on_merging_list_with_non_list() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          field(arg: String): String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          field(arg: [String]): String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "FIELD_ARGUMENT_TYPE_MISMATCH",
            r#"Type of argument "Query.field(arg:)" is incompatible across subgraphs"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn arguments_merges_nullable_and_non_nullable() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          field(arg: String!): String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          field(arg: String): String
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
      field(arg: String!): String
    }
    "###);
}

#[test]
#[ignore = "until merge implementation completed"]
fn arguments_merges_subtypes_within_lists() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          field(arg: [MyInput!]!): String
        }

        interface InputI {
          a: Int
        }

        type ConcreteInputType implements InputI {
          a: Int
          b: Int
        }

        union InputUnion = ConcreteInputType

        input MyInput {
          field: InputUnion!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          field(arg: [MyInput]!): String
        }

        interface InputI {
          a: Int
        }

        type ConcreteInputType implements InputI {
          a: Int
          b: Int
        }

        input MyInput {
          field: InputI
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should merge list element types and nullability
    assert_snapshot!(print_sdl(api_schema.schema()));
}
