use apollo_compiler::ExecutableDocument;
use apollo_compiler::schema::Schema;

use super::compare_operations;
use super::*;
use crate::ValidFederationSchema;

// The schema used in these tests.
const SCHEMA_STR: &str = r#"
    type Query {
        test_i: I!
        test_j: J!
        test_u: U!
        test_v: V!
        test_entity: Entity!
    }

    interface Entity {
        id: ID!
        next: Entity!
    }

    type ObjectA implements Entity {
        id: ID!
        next: ObjectA! # covariant field return
    }

    type ObjectB implements Entity {
        id: ID!
        next: ObjectB! # covariant field return
    }

    type ObjectC implements Entity {
        id: ID!
        next: Entity!
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

fn compare_operation_docs(this: &str, other: &str) -> Result<(), CorrectnessError> {
    let schema = Schema::parse_and_validate(SCHEMA_STR, "schema.graphql").unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    let this_op =
        ExecutableDocument::parse_and_validate(schema.schema(), this, "this.graphql").unwrap();
    let other_op =
        ExecutableDocument::parse_and_validate(schema.schema(), other, "other.graphql").unwrap();
    compare_operations(&schema, &this_op, &other_op)
}

fn assert_compare_operation_docs(this: &str, other: &str) {
    if let Err(err) = compare_operation_docs(this, other) {
        match err {
            CorrectnessError::FederationError(err) => {
                panic!("{err}");
            }
            CorrectnessError::ComparisonError(err) => {
                panic!("compare_operation_docs failed: {err}");
            }
        }
    }
}

#[test]
fn test_basic_pass() {
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    let y = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    compare_operation_docs(x, y).unwrap();
}

#[test]
fn test_basic_fail() {
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    let y = r#"
        query {
            test_i {
                __typename
            }
        }
    "#;
    assert!(compare_operation_docs(x, y).is_err());
}

#[test]
fn test_implied_condition() {
    let x = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                id
            }
        }
    "#;
    let y = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    compare_operation_docs(x, y).unwrap();
}

#[test]
fn test_implied_condition2() {
    let x = r#"
        query($v0: Boolean!, $v1: Boolean!) {
            test_i @include(if: $v0) @skip(if: $v1) {
                id
            }
        }
    "#;
    let y = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                id
            }
        }
    "#;
    compare_operation_docs(x, y).unwrap();
}

#[test]
fn test_boolean_condition_case_split_basic() {
    // x.test_i has no Boolean conditions.
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    // x.test_i has multiple variants split over one variable.
    let y = r#"
        query($v0: Boolean!) {
            test_i {
                id @include(if: $v0)
                id @skip(if: $v0)
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_boolean_condition_case_split_1() {
    // x.test_i has no Boolean conditions.
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    // x.test_i has multiple variants split over one variable.
    let y = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                id
            }
            test_i @skip(if: $v0) {
                id
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_boolean_condition_case_split_2() {
    // x.test_i has a condition with one variable.
    let x = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                id
                data(arg: 0)
                data1: data(arg: 1)
            }
        }
    "#;
    // y.test_i has multiple variants split over two variables.
    let y = r#"
        query($v0: Boolean!, $v1: Boolean!) {
            test_i {
                id
            }
            test_i @include(if: $v0) {
                data(arg: 0)
            }
            ... @include(if: $v1) {
                test_i @include(if: $v0) {
                    data1: data(arg: 1)
                    data2: data(arg: 2) # irrelevant
                }
            }
            test_i @include(if: $v0) @skip(if: $v1) {
                data1: data(arg: 1)
                data3: data(arg: 3) # irrelevant
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_boolean_condition_case_split_3() {
    // x.test_i has no Boolean conditions.
    let x = r#"
        query {
            test_i {
                id
                data(arg: 0)
            }
        }
    "#;
    // y.test_i has multiple variants split over one variable at different levels.
    let y = r#"
        query($v0: Boolean!) {
            test_i {
                id
            }
            test_i @include(if: $v0) {
                data(arg: 0)
            }
            test_i {
                data(arg: 0) @skip(if: $v0)
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_boolean_condition_case_split_4() {
    // x.test_i has no Boolean conditions.
    let x = r#"
        query {
            test_j {
                object(id: "1") {
                    data(arg: 0)
                }
            }
        }
    "#;
    // y.test_i has multiple variants split over one variable at different non-consecutive levels.
    let y = r#"
        query($v0: Boolean!) {
            test_j @include(if: $v0) {
                object(id: "1") {
                    data(arg: 0)
                }
            }
            test_j {
                object(id: "1") {
                    data(arg: 0) @skip(if: $v0)
                }
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_boolean_condition_case_split_5() {
    // x.test_i has a condition with one variable.
    let x = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                id
                data(arg: 0)
                data1: data(arg: 1)
            }
        }
    "#;
    // y.test_i has multiple variants split over two variables.
    let y = r#"
        query($v0: Boolean!, $v1: Boolean!) {
            test_i {
                id
            }
            test_i @include(if: $v0) {
                data(arg: 0)
            }
            test_i @include(if: $v0) {
                data1: data(arg: 1) @include(if: $v1)
            }
            test_i @include(if: $v0) @skip(if: $v1) {
                data1: data(arg: 1)
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_disjunctive_coverage_across_variables() {
    // x requires `id` unconditionally.
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    // y fetches `id` under three mutually exclusive conditions on two
    // variables that form a tautology: ($v0) ∨ ($v1 ∧ ¬$v0) ∨ (¬$v0 ∧ ¬$v1).
    let y = r#"
        query($v0: Boolean!, $v1: Boolean!) {
            test_i @include(if: $v0) {
                id
            }
            ... @skip(if: $v0) {
                test_i @include(if: $v1) {
                    id
                }
                ... @skip(if: $v1) {
                    test_i {
                        id
                    }
                }
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_disjunctive_coverage_incomplete() {
    // x requires `id` unconditionally.
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    // y only fetches `id` under $v0 or ($v1 ∧ ¬$v0), but not when both are
    // false. This must fail — y doesn't cover all cases.
    let y = r#"
        query($v0: Boolean!, $v1: Boolean!) {
            test_i @include(if: $v0) {
                id
            }
            ... @skip(if: $v0) {
                test_i @include(if: $v1) {
                    id
                }
            }
        }
    "#;
    assert!(compare_operation_docs(x, y).is_err());
}

#[test]
fn test_cross_variable_group_coverage() {
    // x requires `id` unconditionally.
    let x = r#"
        query {
            test_i {
                id
            }
        }
    "#;
    // y fetches `id` under four conditions using three variables:
    //   (v0 ∧ v1) ∨ (v0 ∧ ¬v1) ∨ (¬v0 ∧ v2) ∨ (¬v0 ∧ ¬v2) = true
    // The variables are split across variant groups — [v0,v1] vs [v0,v2] —
    // so no single variant's variable group covers all hypotheses. Only the
    // union group [v0,v1,v2] can verify full coverage.
    let y = r#"
        query($v0: Boolean!, $v1: Boolean!, $v2: Boolean!) {
            ... @include(if: $v0) {
                test_i @include(if: $v1) {
                    id
                }
                test_i @skip(if: $v1) {
                    id
                }
            }
            ... @skip(if: $v0) {
                test_i @include(if: $v2) {
                    id
                }
                test_i @skip(if: $v2) {
                    id
                }
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_type_condition_case_split_with_covariant_fields() {
    // x demands `next` at the interface scope, so its sub-selection shape is
    // computed at the `Entity` scope.
    let x = r#"
        query {
            test_entity {
                next {
                    id
                }
            }
        }
    "#;
    // y covers the same demand partitioned by runtime type. Each branch's
    // `next` sub-selection shape lives at the covariantly narrowed object
    // scope (e.g. `ObjectA.next: ObjectA!`). Matching the `ObjectA` case
    // requires remembering that case when comparing the sub-selections:
    // under it, `next` can only be `ObjectA`, so x's `Entity`-scoped child
    // must only be checked against the `ObjectA` case.
    let y = r#"
        query {
            test_entity {
                ... on ObjectA { next { id } }
                ... on ObjectB { next { id } }
                ... on ObjectC { next { id } }
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
    // The two operations are equivalent; the other direction has always worked.
    assert_compare_operation_docs(y, x);
}

#[test]
fn test_type_condition_case_split_with_covariant_fields_nested() {
    // Same as above, but the type-condition case must survive two levels of
    // field traversal: under the `ObjectA` case, both `next` and `next.next`
    // can only be `ObjectA`.
    let x = r#"
        query {
            test_entity {
                next {
                    next {
                        id
                    }
                }
            }
        }
    "#;
    let y = r#"
        query {
            test_entity {
                ... on ObjectA { next { next { id } } }
                ... on ObjectB { next { next { id } } }
                ... on ObjectC { next { next { id } } }
            }
        }
    "#;
    assert_compare_operation_docs(x, y);
}

#[test]
fn test_type_condition_case_split_incomplete_coverage_should_fail() {
    // x demands `next` for every runtime type of `Entity`.
    let x = r#"
        query {
            test_entity {
                next {
                    id
                }
            }
        }
    "#;
    // y misses the `ObjectB` case — this must still fail.
    let y = r#"
        query {
            test_entity {
                ... on ObjectA { next { id } }
                ... on ObjectC { next { id } }
            }
        }
    "#;
    assert!(compare_operation_docs(x, y).is_err());
}

#[test]
fn test_nested_partial_coverage_should_fail() {
    // x requires `data` unconditionally.
    let x = r#"
        query {
            test_i {
                id
                data(arg: 1)
            }
        }
    "#;
    // y's `test_i` variants jointly cover all cases at the top level, but
    // `data` is only fetched when $v0 is true. The per-hypothesis case-split
    // must catch the nested gap under ¬v0.
    let y = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) {
                id
                data(arg: 1)
            }
            test_i @skip(if: $v0) {
                id
            }
        }
    "#;
    assert!(compare_operation_docs(x, y).is_err());
}
