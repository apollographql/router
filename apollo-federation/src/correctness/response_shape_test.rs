use apollo_compiler::ExecutableDocument;
use apollo_compiler::schema::Schema;

use super::*;
use crate::ValidFederationSchema;

// The schema used in these tests.
const SCHEMA_STR: &str = r#"
    type Query {
        test_i: I!
        test_j: J!
        test_u: U!
        test_v: V!
    }

    interface I {
        id: ID!
        data(arg: Int!): String!
    }

    interface J {
        id: ID!
        data(arg: Int!): String!
        object(id: ID!): J!
    }

    type R implements I & J {
        id: ID!
        data(arg: Int!): String!
        object(id: ID!): J!
        r: Int!
    }

    type S implements I & J {
        id: ID!
        data(arg: Int!): String!
        object(id: ID!): J!
        s: Int!
    }

    type T implements I {
        id: ID!
        data(arg: Int!): String!
        t: Int!
    }

    type X implements J {
        id: ID!
        data(arg: Int!): String!
        object(id: ID!): J!
        x: String!
    }

    type Y {
        id: ID!
        y: String!
    }

    type Z implements J {
        id: ID!
        data(arg: Int!): String!
        object(id: ID!): J!
        z: String!
    }

    union U = R | S | X
    union V = R | S | Y

    directive @mod(arg: Int!) on FIELD
"#;

fn response_shape(op_str: &str) -> response_shape::ResponseShape {
    let schema = Schema::parse_and_validate(SCHEMA_STR, "schema.graphql").unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    let op = ExecutableDocument::parse_and_validate(schema.schema(), op_str, "op.graphql").unwrap();
    response_shape::compute_response_shape_for_operation(&op, &schema).unwrap()
}

//=================================================================================================
// Basic response key and alias tests

#[test]
fn test_aliases() {
    let op_str = r#"
        query {
            test_i {
                data(arg: 0)
                alias1: data(arg: 1)
                alias2: data(arg: 1)
            }
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -----> test_i {
        data -----> data(arg: 0)
        alias1 -----> data(arg: 1)
        alias2 -----> data(arg: 1)
      }
    }
    "###);
}

//=================================================================================================
// Type condition tests

#[test]
fn test_type_conditions_over_multiple_different_types() {
    let op_str = r#"
        query {
            test_i {
                ... on R {
                    data(arg: 0)
                }
                ... on S {
                    data(arg: 1)
                }
            }
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -----> test_i {
        data -may-> data(arg: 0) on R
        data -may-> data(arg: 1) on S
      }
    }
    "###);
}

#[test]
fn test_type_conditions_over_multiple_different_interface_types() {
    // These two intersections are distinct type conditions.
    // - `U ∧ I` = {R, S}
    // - `U ∧ J` = `U` = {R, S, X}
    let op_str = r#"
        query {
            test_u {
                ... on I {
                    data(arg: 0)
                }
                ... on J {
                    data(arg: 0)
                }
            }
        }
    "#;
    // Note: The set {R, S} has no corresponding named type definition in the schema, while
    //       `U ∧ J` is just the same as `U`.
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_u -----> test_u {
        data -may-> data(arg: 0) on I ∩ U = {R, S}
        data -may-> data(arg: 0) on U
      }
    }
    "###);
}

#[test]
fn test_type_conditions_merge_same_object_type() {
    // Testing equivalent conditions: `U ∧ R` = `U ∧ I ∧ R` = `U ∧ R ∧ I` = `R`
    // Also, that's different from `U ∧ I` = {R, S}.
    let op_str = r#"
        query {
            test_u {
                ... on R {
                    data(arg: 0)
                }
                ... on I {
                    ... on R {
                        data(arg: 0)
                    }
                }
                ... on R {
                    ... on I {
                        data(arg: 0)
                    }
                }
                ... on I { # different condition
                    data(arg: 0)
                }
            }
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_u -----> test_u {
        data -may-> data(arg: 0) on R
        data -may-> data(arg: 0) on I ∩ U = {R, S}
      }
    }
    "###);
}

#[test]
fn test_type_conditions_merge_equivalent_intersections() {
    // Testing equivalent conditions: `U ∧ I ∧ J` = `U ∧ J ∧ I` = `U ∧ I`= {R, S}
    // Note: The order of applied type conditions is irrelevant.
    let op_str = r#"
        query {
            test_u {
                ... on I {
                    ... on J {
                        data(arg: 0)
                    }
                }
                ... on J {
                    ... on I {
                        data(arg: 0)
                    }
                }
                ... on I {
                    data(arg: 0)
                }
            }
        }
    "#;
    // Note: They are merged into the same condition `I ∧ U`, since that is minimal.
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_u -----> test_u {
        data -may-> data(arg: 0) on I ∩ U = {R, S}
      }
    }
    "###);
}

#[test]
fn test_type_conditions_merge_different_but_equivalent_intersection_expressions() {
    // Testing equivalent conditions: `V ∧ I` = `V ∧ J` = `V ∧ J ∧ I` = {R, S}
    // Note: Those conditions have different sets of types. But, they are still equivalent.
    let op_str = r#"
        query {
            test_v {
                ... on I {
                    data(arg: 0)
                }
                ... on J {
                    data(arg: 0)
                }
                ... on J {
                    ... on I {
                        data(arg: 0)
                    }
                }
            }
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_v -----> test_v {
        data -may-> data(arg: 0) on I ∩ V = {R, S}
      }
    }
    "###);
}

#[test]
fn test_type_conditions_empty_intersection() {
    // Testing unsatisfiable conditions: `U ∧ I ∧ T`= ∅
    let op_str = r#"
        query {
            test_u {
                ... on I {
                    ... on T {
                        infeasible: data(arg: 0)
                    }
                }
            }
        }
    "#;
    // Note: The response shape under `test_u` is empty.
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_u -----> test_u {
      }
    }
    "###);
}

//=================================================================================================
// Boolean condition tests

#[test]
fn test_boolean_conditions_constants() {
    let op_str = r#"
        query {
            test_i {
                # constant true conditions
                merged: data(arg: 0)
                merged: data(arg: 0) @include(if: true)
                merged: data(arg: 0) @skip(if: false)

                # constant false conditions
                infeasible_1: data(arg: 0) @include(if: false)
                infeasible_2: data(arg: 0) @skip(if: true)
            }
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -----> test_i {
        merged -----> data(arg: 0)
      }
    }
    "###);
}

#[test]
fn test_boolean_conditions_different_multiple_conditions() {
    let op_str = r#"
        query($v0: Boolean!, $v1: Boolean!, $v2: Boolean!) {
            test_i @include(if: $v0) {
                data(arg: 0)
                data(arg: 0) @include(if: $v1)
                ... @include(if: $v1) {
                    data(arg: 0) @include(if: $v2)
                }
            }
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v0 {
        data -may-> data(arg: 0)
        data -may-> data(arg: 0) if v1
        data -may-> data(arg: 0) if v1 ∧ v2
      }
    }
    "###);
}

#[test]
fn test_boolean_conditions_unsatisfiable_conditions() {
    let op_str = r#"
        query($v0: Boolean!, $v1: Boolean!) {
            test_i @include(if: $v0) {
                # conflict directly within the field directives
                infeasible_1: data(arg: 0) @include(if: $v1) @skip(if: $v1)
                # conflicting with the parent inline fragment
                ... @skip(if: $v1) {
                    infeasible_2: data(arg: 0) @include(if: $v1)
                }
                infeasible_3: data(arg: 0) @skip(if: $v0) # conflicting with the parent-selection condition
            }
        }
    "#;
    // Note: The response shape under `test_i` is empty.
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v0 {
      }
    }
    "###);
}

//=================================================================================================
// Non-conditional directive tests

#[test]
fn test_non_conditional_directives() {
    let op_str = r#"
        query {
            test_i {
                data(arg: 0) @mod(arg: 0) # different only in directives
                data(arg: 0) @mod(arg: 1) # different only in directives
                data(arg: 0) # no directives
            }
        }
    "#;
    // Note: All `data` definitions are merged, but the first selection (in depth-first order) is
    //       chosen as the representative.
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -----> test_i {
        data -----> data(arg: 0) @mod(arg: 0)
      }
    }
    "###);
}

//=================================================================================================
// Fragment spread tests

#[test]
fn test_fragment_spread() {
    let op_str = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                merge_1: data(arg: 0)
                ...F
            }
        }

        fragment F on I {
            merge_1: data(arg: 0)
            from_fragment: data(arg: 0)
            infeasible_1: data(arg: 0) @skip(if: $v0)
        }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v0 {
        merge_1 -----> data(arg: 0)
        from_fragment -----> data(arg: 0)
      }
    }
    "###);
}

//=================================================================================================
// Sub-selection merging tests

#[test]
fn test_merge_sub_selection_sets() {
    let op_str = r#"
    query($v0: Boolean!, $v1: Boolean!) {
        test_j {
            object(id: 0) {
                merged_1: data(arg: 0)
                ... on R {
                    merged_2: data(arg: 0)
                }
                merged_3: data(arg: 0) @include(if: $v0)
            }
            object(id: 0) {
                merged_1: data(arg: 0)
                ... on S {
                    merged_2: data(arg: 1)
                }
                merged_3: data(arg: 0) @include(if: $v1)
            }
            object(id: 0) {
                merged_3: data(arg: 0) # no condition
            }
        }
    }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_j -----> test_j {
        object -----> object(id: 0) {
          merged_1 -----> data(arg: 0)
          merged_2 -may-> data(arg: 0) on R
          merged_2 -may-> data(arg: 1) on S
          merged_3 -may-> data(arg: 0) if v0
          merged_3 -may-> data(arg: 0) if v1
          merged_3 -may-> data(arg: 0)
        }
      }
    }
    "###);
}

#[test]
fn test_not_merge_sub_selection_sets_under_different_type_conditions() {
    let op_str = r#"
    query {
        test_j {
            object(id: 0) {
                unmerged: data(arg: 0)
            }
            # unmerged due to parents with different type conditions
            ... on R {
                object(id: 0) {
                    unmerged: data(arg: 0)
                }
            }
        }
    }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_j -----> test_j {
        object -may-> object(id: 0) on J {
          unmerged -----> data(arg: 0)
        }
        object -may-> object(id: 0) on R {
          unmerged -----> data(arg: 0)
        }
      }
    }
    "###);
}

#[test]
fn test_merge_sub_selection_sets_with_boolean_conditions() {
    let op_str = r#"
    query($v0: Boolean!, $v1: Boolean!) {
        test_j {
            object(id: 0) @include(if: $v0) {
                merged: data(arg: 0)
                unmerged: data(arg: 0)
            }
            object(id: 0) @include(if: $v0) {
                merged: data(arg: 0) @include(if: $v0)
            }
            # unmerged due to parents with different Boolean conditions
            object(id: 0) @include(if: $v1) {
                unmerged: data(arg: 0)
            }
        }
    }
    "#;
    insta::assert_snapshot!(response_shape(op_str), @r###"
    {
      test_j -----> test_j {
        object -may-> object(id: 0) if v0 {
          merged -----> data(arg: 0)
          unmerged -----> data(arg: 0)
        }
        object -may-> object(id: 0) if v1 {
          unmerged -----> data(arg: 0)
        }
      }
    }
    "###);
}
