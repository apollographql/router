use apollo_compiler::coord;
use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;

// =============================================================================
// ENUM TYPES - Tests for enum type merging behavior
// =============================================================================

#[test]
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
    let enum_e = coord!(E)
        .lookup(api_schema.schema())
        .expect("Enum E should exist");
    assert_snapshot!(enum_e, @r#"
        enum E {
          V1
          V2
        }
        "#);
}

#[test]
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
    let enum_e = coord!(E)
        .lookup(api_schema.schema())
        .expect("Enum E should exist");
    assert_snapshot!(enum_e, @r###"
        enum E {
          V1
        }
        "###);
}

#[test]
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
    let enum_e = coord!(E)
        .lookup(api_schema.schema())
        .expect("Enum E should exist");
    assert_snapshot!(enum_e, @r#"
        enum E {
          V1
          V3
        }
        "#);
}

#[test]
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
    let enum_e = coord!(E)
        .lookup(api_schema.schema())
        .expect("Enum E should exist");
    assert_snapshot!(enum_e, @r#"
        enum E {
          V1
          V2
        }
        "#);
}

// =============================================================================
// INPUT TYPES - Tests for input type merging behavior
// =============================================================================

#[test]
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
    assert!(
        coord!(A.x).lookup_input_field(api_schema.schema()).is_ok(),
        "Expected A.x to exist"
    );
    assert!(
        coord!(A.y).lookup_input_field(api_schema.schema()).is_err(),
        "Expected A.y to be undefined"
    );
}

#[test]
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
    let target = coord!(A.x)
        .lookup_input_field(api_schema.schema())
        .expect("Expected A.x to exist");
    assert_eq!(
        target.ty.to_string(),
        "Int!",
        "Expected A.x to be of type Int! but got {}",
        target.ty
    );
}

#[test]
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
    let union_u = coord!(U)
        .lookup(api_schema.schema())
        .expect("Union U should exist");
    assert_snapshot!(union_u, @"union U = A | B | C");
}

// =============================================================================
// Handling extension and non-extension definitions
// =============================================================================

#[test]
fn empty_object_type_definition_with_extension_in_subgraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
            type T # empty type definition

            extend type T { # an extension with a field
                field: Boolean
            }

            type Query {
                test: T
            }
        "#,
    };

    // This used to panic with a ExtensionWithNoBase error.
    compose_as_fed2_subgraphs(&[subgraph_a]).expect("composing subgraphs");
}
