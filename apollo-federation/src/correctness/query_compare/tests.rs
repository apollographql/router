use apollo_compiler::ExecutableDocument;
use apollo_compiler::schema::Schema;

use super::*;
use crate::ValidFederationSchema;

const SCHEMA_STR: &str = r#"
    type Query {
        test_i: I!
        test_entity: Entity!
    }

    interface Entity {
        id: ID!
        next: Entity!
    }

    type ObjectA implements Entity {
        id: ID!
        next: ObjectA!
    }

    type ObjectB implements Entity {
        id: ID!
        next: ObjectB!
    }

    interface I {
        id: ID!
        data(arg: Int!): String!
    }

    type R implements I {
        id: ID!
        data(arg: Int!): String!
        r: Int!
    }

    type S implements I {
        id: ID!
        data(arg: Int!): String!
        s: Int!
    }

    directive @mod(arg: Int!) on FIELD
"#;

fn check(left: &str, right: &str) -> Result<(), ComparisonError> {
    let schema = Schema::parse_and_validate(SCHEMA_STR, "schema.graphql").unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    let left =
        ExecutableDocument::parse_and_validate(schema.schema(), left, "left.graphql").unwrap();
    let right =
        ExecutableDocument::parse_and_validate(schema.schema(), right, "right.graphql").unwrap();
    includes(&schema, &left, &right)
}

fn error(left: &str, right: &str) -> ComparisonError {
    match check(left, right) {
        Ok(()) => panic!("expected the comparison to fail"),
        Err(err) => err,
    }
}

#[test]
fn identical_queries_are_included() {
    let q = r#"{ test_i { id } }"#;
    check(q, q).unwrap();
}

#[test]
fn extra_left_fields_are_fine() {
    check(r#"{ test_i { id data(arg: 1) } }"#, r#"{ test_i { id } }"#).unwrap();
}

#[test]
fn missing_field_is_reported_with_its_path() {
    insta::assert_snapshot!(
        error(r#"{ test_i { id } }"#, r#"{ test_i { id data(arg: 1) } }"#),
        @r###"
    left does not include right
      in response name: test_i
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: data
      over runtime types: {R, S}
      --> left does not select `data` on R
    "###
    );
}

#[test]
fn different_arguments_are_reported() {
    insta::assert_snapshot!(
        error(r#"{ test_i { data(arg: 1) } }"#, r#"{ test_i { data(arg: 2) } }"#),
        @r###"
    left does not include right
      in response name: test_i
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: data
      over runtime types: {R, S}
      --> `data` is called with different arguments
        left:  (arg: 1)
        right: (arg: 2)
    "###
    );
}

#[test]
fn aliased_field_resolving_a_different_field_is_reported() {
    insta::assert_snapshot!(
        error(r#"{ test_i { x: id } }"#, r#"{ test_i { x: data(arg: 1) } }"#),
        @r###"
    left does not include right
      in response name: test_i
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: x
      over runtime types: {R, S}
      --> `x` resolves a different field: left selects `id`, right selects `data`
    "###
    );
}

// An unconditional left field covers a conditional right field: the left clause `true`
// symbolically covers both branches of `$v0`.
#[test]
fn unconditional_left_covers_conditional_right() {
    check(
        r#"{ test_i { id } }"#,
        r#"query($v0: Boolean!) { test_i { id @include(if: $v0) } }"#,
    )
    .unwrap();
}

// Complementary clauses on the left jointly cover an unconditional right field. This is the
// clause-subtraction case the scalar shortcut exists for.
#[test]
fn complementary_left_clauses_cover_unconditional_right() {
    check(
        r#"query($v0: Boolean!) {
            test_i { id @include(if: $v0) }
            test_i { id @skip(if: $v0) }
        }"#,
        r#"{ test_i { id } }"#,
    )
    .unwrap();
}

#[test]
fn conditional_left_does_not_cover_unconditional_right() {
    insta::assert_snapshot!(
        error(
            r#"query($v0: Boolean!) { test_i { id @include(if: $v0) } }"#,
            r#"{ test_i { id } }"#,
        ),
        @r###"
    left does not include right
      in response name: test_i
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: id
      assuming: ¬$v0
      over runtime types: {R, S}
      --> left does not select `id` on R
    "###
    );
}

// The left covers `test_i` under every assignment at the top level, but `data` is only selected
// when `$v0` holds. The per-assignment split has to catch the nested gap.
#[test]
fn nested_gap_under_one_assignment_is_reported() {
    insta::assert_snapshot!(
        error(
            r#"query($v0: Boolean!) {
                test_i @include(if: $v0) { id data(arg: 1) }
                test_i @skip(if: $v0) { id }
            }"#,
            r#"{ test_i { id data(arg: 1) } }"#,
        ),
        @r###"
    left does not include right
      in response name: test_i
      assuming: ¬$v0
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: data
      over runtime types: {R, S}
      --> left does not select `data` on R
    "###
    );
}

// Type conditions restrict which runtime types a selection reaches; a case the left omits must
// be reported against that runtime type.
#[test]
fn missing_type_condition_case_is_reported() {
    insta::assert_snapshot!(
        error(
            r#"{ test_i { ... on R { id } } }"#,
            r#"{ test_i { id } }"#,
        ),
        @r###"
    left does not include right
      in response name: test_i
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: id
      over runtime types: {S}
      --> left does not select `id` on S
    "###
    );
}

#[test]
fn type_conditions_covering_every_case_are_included() {
    check(
        r#"{ test_i { ... on R { id } ... on S { id } } }"#,
        r#"{ test_i { id } }"#,
    )
    .unwrap();
}

// A covariant return must not be widened back to the declared interface: `next` on ObjectA
// returns ObjectA, so the left only has to cover ObjectA's fields there.
#[test]
fn covariant_return_narrows_the_child_region() {
    check(
        r#"{
            test_entity {
                ... on ObjectA { next { id } }
                ... on ObjectB { next { id } }
            }
        }"#,
        r#"{ test_entity { next { id } } }"#,
    )
    .unwrap();
}

#[test]
fn unsupported_directive_is_not_an_inclusion_finding() {
    let err = error(r#"{ test_i { id } }"#, r#"{ test_i { id @mod(arg: 1) } }"#);
    assert!(!err.is_inclusion_finding(), "{err}");
}

#[test]
fn shared_variable_declarations_must_agree() {
    let err = error(
        r#"query($v0: Boolean! = true) { test_i { id @include(if: $v0) } }"#,
        r#"query($v0: Boolean! = false) { test_i { id @include(if: $v0) } }"#,
    );
    assert!(err.is_inclusion_finding(), "{err}");
    insta::assert_snapshot!(err, @r###"
    left does not include right
      --> variable $v0 is declared differently
        left:  Boolean! = true
        right: Boolean! = false
    "###);
}

#[test]
fn error_renders_as_json() {
    let err = error(
        r#"{ test_i { data(arg: 1) } }"#,
        r#"{ test_i { data(arg: 2) } }"#,
    );
    insta::assert_snapshot!(serde_json::to_string_pretty(&err.to_json()).unwrap(), @r###"
    {
      "path": [
        {
          "kind": "response_name",
          "name": "test_i"
        },
        {
          "kind": "type_region",
          "types": [
            "Query"
          ]
        },
        {
          "kind": "child_selection",
          "field": "test_i",
          "possible_types": [
            "R",
            "S"
          ]
        },
        {
          "kind": "response_name",
          "name": "data"
        },
        {
          "kind": "type_region",
          "types": [
            "R",
            "S"
          ]
        }
      ],
      "reason": {
        "reason": "field_arguments_mismatch",
        "response_name": "data",
        "field_name": "data",
        "left": "(arg: 1)",
        "right": "(arg: 2)"
      }
    }
    "###);
}
