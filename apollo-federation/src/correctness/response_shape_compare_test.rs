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
