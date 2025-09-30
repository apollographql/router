use insta::assert_snapshot;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;

// =============================================================================
// ENUM TYPES - Tests for enum type merging behavior
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_merges_inconsistent_enum_only_used_as_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          e: E!
        }

        enum E {
          V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should merge to include both V1 and V2 values (JS test only checks the enum type, not full schema)
    let enum_e = api_schema
        .schema()
        .types
        .get("E")
        .expect("Enum E should exist");
    if let apollo_compiler::schema::ExtendedType::Enum(enum_type) = enum_e {
        let enum_string = enum_type.to_string();
        assert_snapshot!(enum_string, @r###"
        enum E {
          V1
          V2
        }
        "###);
    } else {
        panic!("E should be an enum type");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_merges_enum_but_skips_inconsistent_values_only_used_as_input() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(e: E!): Int
        }

        enum E {
          V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V1
          V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should only include V1 (common value), V2 is skipped for input-only enum
    let enum_e = api_schema
        .schema()
        .types
        .get("E")
        .expect("Enum E should exist");
    if let apollo_compiler::schema::ExtendedType::Enum(enum_type) = enum_e {
        let enum_string = enum_type.to_string();
        assert_snapshot!(enum_string, @r###"
        enum E {
          V1
        }
        "###);
    } else {
        panic!("E should be an enum type");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_does_not_error_if_skipped_inconsistent_value_has_directive() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(e: E!): Int
        }

        enum E {
          V1
          V2 @deprecated(reason: "use V3 instead")
          V3
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V1
          V3
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should include V1 and V3 (common values), V2 is skipped but no error for @deprecated on V2
    let enum_e = api_schema
        .schema()
        .types
        .get("E")
        .expect("Enum E should exist");
    if let apollo_compiler::schema::ExtendedType::Enum(enum_type) = enum_e {
        let enum_string = enum_type.to_string();
        assert_snapshot!(enum_string, @r###"
        enum E {
          V1
          V3
        }
        "###);
    } else {
        panic!("E should be an enum type");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_errors_if_enum_used_only_as_input_has_no_consistent_values() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(e: E!): Int
        }

        enum E {
          V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "EMPTY_MERGED_ENUM_TYPE",
            r#"None of the values of enum type "E" are defined consistently in all the subgraphs defining that type. As only values common to all subgraphs are merged, this would result in an empty type."#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_errors_when_merging_inconsistent_enum_used_as_both_input_and_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          e: E!
          f(e: E!): Int
        }

        enum E {
          V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[
            (
                "ENUM_VALUE_MISMATCH",
                r#"Enum type "E" is used as both input type (for example, as type of "Query.f(e:)") and output type (for example, as type of "Query.e"), but value "V1" is not defined in all the subgraphs defining "E": "V1" is defined in subgraph "subgraphA" but not in subgraph "subgraphB""#,
            ),
            (
                "ENUM_VALUE_MISMATCH",
                r#"Enum type "E" is used as both input type (for example, as type of "Query.f(e:)") and output type (for example, as type of "Query.e"), but value "V2" is not defined in all the subgraphs defining "E": "V2" is defined in subgraph "subgraphB" but not in subgraph "subgraphA""#,
            ),
        ],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_ignores_inaccessible_fields_when_merging_enums_used_as_both_input_and_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          e: E!
          f(e: E!): Int
        }

        enum E {
          V1
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V1
          V2 @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph =
        result.expect("Expected composition to succeed - should ignore @inaccessible enum values");
}

#[test]
#[ignore = "until merge implementation completed"]
fn enum_types_succeeds_merging_consistent_enum_used_as_both_input_and_output() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(e: E!): E!
        }

        enum E {
          V1
          V2
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        enum E {
          V1
          V2
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should include both V1 and V2 (fully consistent)
    let enum_e = api_schema
        .schema()
        .types
        .get("E")
        .expect("Enum E should exist");
    if let apollo_compiler::schema::ExtendedType::Enum(enum_type) = enum_e {
        let enum_string = enum_type.to_string();
        assert_snapshot!(enum_string, @r###"
        enum E {
          V1
          V2
        }
        "###);
    } else {
        panic!("E should be an enum type");
    }
}

// =============================================================================
// INPUT TYPES - Tests for input type merging behavior
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn input_types_only_merges_fields_common_to_all_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q1(a: A): String
        }

        input A {
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          q2(a: A): String
        }

        input A {
          x: Int
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should only include field 'x' (common to both subgraphs), field 'y' should be undefined
    let input_a = api_schema
        .schema()
        .types
        .get("A")
        .expect("Input A should exist");
    if let apollo_compiler::schema::ExtendedType::InputObject(input_type) = input_a {
        // Validate field 'x' exists
        assert!(
            input_type.fields.get("x").is_some(),
            "Expected field 'x' to exist on input A"
        );
        // Validate field 'y' does not exist (not common to both subgraphs)
        assert!(
            input_type.fields.get("y").is_none(),
            "Expected field 'y' to be undefined on input A"
        );
    } else {
        panic!("A should be an input type");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
fn input_types_merges_input_field_with_different_but_compatible_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q1(a: A): String
        }

        input A {
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          q2(a: A): String
        }

        input A {
          x: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should merge to non-nullable (Int!) for input compatibility
    let input_a = api_schema
        .schema()
        .types
        .get("A")
        .expect("Input A should exist");
    if let apollo_compiler::schema::ExtendedType::InputObject(input_type) = input_a {
        if let Some(x_field) = input_type.fields.get("x") {
            // Check that field type is Int! (non-nullable)
            assert!(
                x_field.ty.to_string() == "Int!",
                "Expected field type to be Int!, got {}",
                x_field.ty
            );
        } else {
            panic!("Expected field 'x' to exist on input A");
        }
    } else {
        panic!("A should be an input type");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
fn input_types_errors_when_merging_completely_inconsistent_input_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          f(i: MyInput!): Int
        }

        input MyInput {
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        input MyInput {
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "EMPTY_MERGED_INPUT_TYPE",
            r#"None of the fields of input object type "MyInput" are consistently defined in all the subgraphs defining that type. As only fields common to all subgraphs are merged, this would result in an empty type."#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn input_types_errors_if_mandatory_input_field_not_in_all_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q1(a: A): String
        }

        input A {
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          q2(a: A): String
        }

        input A {
          x: Int
          y: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH",
            r#"Input object field "A.y" is required in some subgraphs but does not appear in all subgraphs: it is required in subgraph "subgraphB" but does not appear in subgraph "subgraphA""#,
        )],
    );
}

// =============================================================================
// UNION TYPES - Tests for union type merging behavior
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn union_types_merges_inconsistent_unions() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          u: U!
        }

        union U = A | B

        type A {
          a: Int
        }

        type B {
          b: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        union U = C

        type C {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");

    // Should merge to include A, B, and C in union
    let union_u = api_schema
        .schema()
        .types
        .get("U")
        .expect("Union U should exist");
    if let apollo_compiler::schema::ExtendedType::Union(union_type) = union_u {
        let union_string = union_type.to_string();
        assert_snapshot!(union_string, @"union U = A | B | C");
    } else {
        panic!("U should be a union type");
    }
}
