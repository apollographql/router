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

    directive @mod(arg: Int!) on FIELD | INLINE_FRAGMENT | FRAGMENT_SPREAD
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

// The three cases `Tests/GraphQL/Theories/QueryInclusion.lean` added for the model's
// `guardedFieldGroupSymbolicallyIncludesWithFuel`, transcribed onto this schema. The model states
// them over the rule directly; here they go through `includes`, which reaches the same rule.

// `guardedFieldGroup_symbolicIndependentChildrenSmoke`: two independent guards share a parent
// response name but belong to different child response names.
#[test]
fn independent_guards_sharing_a_parent_response_name() {
    let query = r#"query($leftBranch: Boolean!, $rightBranch: Boolean!) {
        test_i @include(if: $leftBranch) { left_name: id }
        test_i @include(if: $rightBranch) { right_name: id }
    }"#;
    check(query, query).unwrap();
}

// `guardedFieldGroup_symbolicRejectsGuardedCoverageGapSmoke`: a conditional occurrence cannot
// cover an unconditional one, so the witness declines and the fallback reaches the rejection.
#[test]
fn guarded_parent_does_not_cover_unconditional_parent() {
    insta::assert_snapshot!(
        error(
            r#"query($leftBranch: Boolean!) { test_i @include(if: $leftBranch) { id } }"#,
            r#"{ test_i { id } }"#,
        ),
        @r###"
    left does not include right
      in response name: test_i
      assuming: ¬$leftBranch
      over runtime types: {Query}
      --> left does not select `test_i` on Query
    "###
    );
}

// `guardedFieldGroup_symbolicDropsContradictoryChildSmoke`: the right child's `@skip` contradicts
// the parent occurrence's `@include`, so that contribution is unreachable and needs no cover.
#[test]
fn contradictory_right_child_needs_no_cover() {
    check(
        r#"query($leftBranch: Boolean!) {
            test_i @include(if: $leftBranch) { id }
        }"#,
        r#"query($leftBranch: Boolean!) {
            test_i @include(if: $leftBranch) {
                id
                impossible: id @skip(if: $leftBranch)
            }
        }"#,
    )
    .unwrap();
}

// The shape `group_symbolically_includes` exists for: one composite response name carrying
// several guarded occurrences, whose children are guarded independently of each other. The rule
// carries each occurrence's guard into the child boundary, so `$v0` and `$v1` are decided where
// they are read instead of being multiplied out here.
#[test]
fn guarded_occurrences_merge_into_the_child_boundary() {
    check(
        r#"query($v0: Boolean!, $v1: Boolean!) {
            test_i { id }
            test_i @include(if: $v0) { data(arg: 1) }
            test_i @include(if: $v1) { r_or_s: __typename }
        }"#,
        r#"query($v0: Boolean!, $v1: Boolean!) {
            test_i {
                id
                data(arg: 1) @include(if: $v0)
                r_or_s: __typename @include(if: $v1)
            }
        }"#,
    )
    .unwrap();
}

// The same shape, with the right side asking unconditionally for what the left only fetches under
// `$v0`. Carrying the guard down must not lose it.
#[test]
fn guarded_left_occurrence_does_not_cover_unconditional_child() {
    insta::assert_snapshot!(
        error(
            r#"query($v0: Boolean!) {
                test_i { id }
                test_i @include(if: $v0) { data(arg: 1) }
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

// A child guard contradicting the occupancy it is carried under makes that child unreachable, so
// it covers nothing. Seeding the child boundary has to reach the same conclusion the assignment
// search does by finding the occurrence inactive.
#[test]
fn child_guard_contradicting_its_occurrence_covers_nothing() {
    insta::assert_snapshot!(
        error(
            r#"query($v0: Boolean!) {
                test_i { id }
                test_i @include(if: $v0) { data(arg: 1) @skip(if: $v0) }
            }"#,
            r#"query($v0: Boolean!) { test_i { id data(arg: 1) @include(if: $v0) } }"#,
        ),
        @r###"
    left does not include right
      in response name: test_i
      assuming: $v0
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: data
      over runtime types: {R, S}
      --> left does not select `data` on R
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

// A directive the model gives no semantics to is still part of the resolver call, so two fields
// differing only by one are not the same call.
#[test]
fn custom_directives_are_compared() {
    let err = error(r#"{ test_i { id } }"#, r#"{ test_i { id @mod(arg: 1) } }"#);
    assert!(err.is_inclusion_finding(), "{err}");
    insta::assert_snapshot!(err, @r###"
    left does not include right
      in response name: test_i
      over runtime types: {Query}
      in sub-selection of: test_i -> {R, S}
      in response name: id
      over runtime types: {R, S}
      --> `id` resolves `id` with different directives
        left:  (none)
        right: @mod
    "###);
}

#[test]
fn matching_custom_directives_are_included() {
    check(
        r#"{ test_i { id @mod(arg: 1) } }"#,
        r#"{ test_i { id @mod(arg: 1) } }"#,
    )
    .unwrap();
}

#[test]
fn custom_directive_arguments_are_compared() {
    let err = error(
        r#"{ test_i { id @mod(arg: 1) } }"#,
        r#"{ test_i { id @mod(arg: 2) } }"#,
    );
    assert!(err.is_inclusion_finding(), "{err}");
}

// `__typename` is selectable on every composite type and declared by none of them.
#[test]
fn typename_is_selectable() {
    check(
        r#"{ test_i { __typename id } }"#,
        r#"{ test_i { __typename } }"#,
    )
    .unwrap();
}

#[test]
fn missing_typename_is_reported() {
    let err = error(r#"{ test_i { id } }"#, r#"{ test_i { __typename } }"#);
    assert!(err.is_inclusion_finding(), "{err}");
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

//==================================================================================================
// The subgraph oracle's state

/// A supergraph where two implementations reach the same child region through different subgraphs.
///
/// `P.f` resolves only in subgraph A and `Q.f` only in B, and both return `R`, so the two child
/// obligations agree on the region `{R}` and disagree on which subgraphs remain possible. `R.g`
/// returns an interface implemented by `X` in A and by `Y` in B, so that disagreement decides what
/// the next level admits.
const CONSTRAINT_STATE_SCHEMA: &str = include_str!("testdata/constraint_state.graphql");

fn constraint_state_fixture() -> (
    crate::schema::ValidFederationSchema,
    crate::schema::ValidFederationSchema,
    apollo_compiler::collections::IndexMap<
        std::sync::Arc<str>,
        crate::schema::ValidFederationSchema,
    >,
) {
    let supergraph =
        crate::Supergraph::new_with_router_specs(CONSTRAINT_STATE_SCHEMA).expect("valid fixture");
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("api schema");
    let subgraphs = supergraph
        .extract_subgraphs()
        .expect("subgraphs")
        .into_iter()
        .map(|(name, subgraph)| (name, subgraph.schema))
        .collect();
    (supergraph.schema.clone(), api_schema, subgraphs)
}

/// Deciding `left` includes `right` under the federated oracle, as the plan checker does.
fn includes_in_supergraph(left: &str, right: &str) -> Result<(), ComparisonError> {
    let (supergraph_schema, api_schema, subgraphs) = constraint_state_fixture();
    let parse = |source: &str| {
        ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            source.to_string(),
            "op.graphql",
        )
        .expect("valid operation")
    };
    let constraint = crate::correctness::subgraph_constraint::SubgraphConstraint::new(&subgraphs);
    includes_with_constraint(&supergraph_schema, &constraint, &parse(left), &parse(right))
}

/// The right side asks for a field only `Y` has, and the left never selects it, so this is not an
/// inclusion. `Y` is reachable only through subgraph B, and only the `Q` branch keeps B possible --
/// so answering correctly depends on the `Q` obligation surviving as far as `g`.
#[test]
fn an_obligation_reached_through_a_different_subgraph_is_not_dropped() {
    let error = includes_in_supergraph(
        "{ items { f { g { __typename } } } }",
        "{ items { f { g { ... on Y { y } } } } }",
    )
    .expect_err("left does not select `y` on Y");
    assert!(
        error.to_string().contains('y'),
        "expected the missing field to be named, got:\n{error}"
    );
}

/// The same shape through the other branch, so a fix cannot pass by keeping only one of the two.
#[test]
fn an_obligation_reached_through_the_other_subgraph_is_not_dropped() {
    includes_in_supergraph(
        "{ items { f { g { __typename } } } }",
        "{ items { f { g { ... on X { x } } } } }",
    )
    .expect_err("left does not select `x` on X");
}

/// The control: what the left does select is included, so the tests above are not passing merely
/// because this fixture rejects everything.
#[test]
fn what_the_left_selects_is_included() {
    includes_in_supergraph(
        "{ items { f { g { __typename } } } }",
        "{ items { f { g { __typename } } } }",
    )
    .expect("an operation includes itself");
}

//==================================================================================================
// How a selection is packaged

/// How many response-name groups deciding this pair costs. A shortcut that fires settles a group
/// without walking what is under it, so this distinguishes "the same verdict" from "the same work"
/// -- and the shortcut's own result is what these tests are about.
fn groups_to_decide(left: &str, right: &str) -> u64 {
    let before = groups_decided();
    check(left, right).expect("expected inclusion");
    groups_decided() - before
}

// `... on I` inside a field of type `I` admits everything the position holds, so it selects what
// its body selects. The three spellings are the same selection, and must cost the same to decide.
#[test]
fn a_vacuous_wrapper_costs_what_its_body_costs() {
    let bare = groups_to_decide(r#"{ test_i { id } }"#, r#"{ test_i { id } }"#);
    let inline = groups_to_decide(
        r#"{ test_i { ... on I { id } } }"#,
        r#"{ test_i { ... on I { id } } }"#,
    );
    let named = groups_to_decide(
        r#"{ test_i { ...F } } fragment F on I { id }"#,
        r#"{ test_i { ...F } } fragment F on I { id }"#,
    );
    assert_eq!(
        (bare, inline),
        (bare, bare),
        "an inline wrapper changed the work"
    );
    assert_eq!(
        (bare, named),
        (bare, bare),
        "a named spread changed the work"
    );
}

// The same, across spellings: one side wrapped and the other bare must still match, and still cost
// what the bare pair costs. This is the shape a query plan takes against the operation it is
// checked against, and getting it wrong is invisible in the verdict alone.
#[test]
fn a_vacuous_wrapper_matches_the_bare_body() {
    for (left, right) in [
        (r#"{ test_i { ... on I { id } } }"#, r#"{ test_i { id } }"#),
        (r#"{ test_i { id } }"#, r#"{ test_i { ... on I { id } } }"#),
        (
            r#"{ test_i { ...F } } fragment F on I { id }"#,
            r#"{ test_i { id } }"#,
        ),
        (
            r#"{ test_i { id } }"#,
            r#"{ test_i { ...F } } fragment F on I { id }"#,
        ),
        (
            r#"{ test_i { ...F } } fragment F on I { id }"#,
            r#"{ test_i { ... on I { id } } }"#,
        ),
    ] {
        let cost = groups_to_decide(left, right);
        let bare = groups_to_decide(r#"{ test_i { id } }"#, r#"{ test_i { id } }"#);
        assert_eq!(cost, bare, "packaging changed the work:\n{left}\n{right}");
    }
}

// Two transparent wrappers with different names are still both transparent.
#[test]
fn differently_named_wrappers_match() {
    check(
        r#"{ test_i { ...Left } } fragment Left on I { id }"#,
        r#"{ test_i { ...Right } } fragment Right on I { id }"#,
    )
    .unwrap();
}

// A condition that does narrow must not be flattened away. `... on R` keeps `id` off `S`, so a
// right side asking for `id` across the whole position is not included.
#[test]
fn a_narrowing_wrapper_is_not_flattened() {
    insta::assert_snapshot!(
        error(r#"{ test_i { ... on R { id } } }"#, r#"{ test_i { id } }"#),
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

// A directive is uninterpreted, so a wrapper carrying one is never *nothing*, even where its type
// condition admits the whole position. The verdict does not show this -- the general search drops
// an unmodeled directive on a fragment, so it accepts either way -- but the work does: the
// shortcut declines and the search runs.
#[test]
fn a_wrapper_carrying_a_directive_is_not_transparent() {
    let bare = groups_to_decide(r#"{ test_i { id } }"#, r#"{ test_i { id } }"#);
    let carrying = groups_to_decide(
        r#"{ test_i { ... on I @mod(arg: 1) { id } } }"#,
        r#"{ test_i { id } }"#,
    );
    assert!(
        carrying > bare,
        "the shortcut treated a directive-carrying wrapper as transparent \
         ({carrying} groups against {bare} for the bare pair)"
    );
}

// A covariant return narrows what the child position holds, so a wrapper transparent at the
// interface's position need not be transparent at the narrower one.
#[test]
fn a_wrapper_is_read_against_the_narrowed_child_position() {
    check(
        r#"{ test_entity { next { ... on Entity { id } } } }"#,
        r#"{ test_entity { next { id } } }"#,
    )
    .unwrap();
}
