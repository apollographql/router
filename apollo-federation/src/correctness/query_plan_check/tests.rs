//! Plan-level tests for the checker.
//!
//! Each case plans a real operation with the real planner and checks the result, so what is under
//! test is the whole walk — entity fetches, `@key` and `@requires` matching, and the completeness
//! comparison — rather than a hand-built plan that may not be a shape the planner emits.
//!
//! The supergraph beside this file is composed by `compose_fixture` in the fuzz package from the
//! subgraph SDL written there, so the fixture has a visible source. It is deliberately close to
//! the shape a corpus graph had when it found a defect here: an entity whose `@requires` field set
//! wraps its body in a type condition that is already the field's own type.

use apollo_compiler::ExecutableDocument;

use super::super::CorrectnessError;
use crate::Supergraph;
use crate::query_plan::query_planner::QueryPlanner;

const SUPERGRAPH: &str = include_str!("testdata/entity_requires.graphql");

fn planner() -> QueryPlanner {
    let supergraph = Supergraph::new_with_router_specs(SUPERGRAPH).expect("valid fixture");
    QueryPlanner::new(&supergraph, Default::default()).expect("planner")
}

/// Plans `planned` and checks that plan against `checked`. Passing two different operations is how
/// a wrong plan is produced without hand-editing one: a plan is only correct for what it planned.
fn check_plan_of(planned: &str, checked: &str) -> Result<(), CorrectnessError> {
    let planner = planner();
    let parse = |source: &str| {
        ExecutableDocument::parse_and_validate(
            planner.api_schema().schema(),
            source.to_string(),
            "operation.graphql",
        )
        .expect("valid operation")
    };
    let plan = planner
        .build_query_plan(&parse(planned), None, Default::default())
        .expect("query plan");
    crate::correctness::check_plan(
        planner.api_schema(),
        planner.supergraph_schema(),
        planner.subgraph_schemas(),
        &parse(checked),
        &plan,
    )
}

fn check(operation: &str) -> Result<(), CorrectnessError> {
    check_plan_of(operation, operation)
}

fn rejection(planned: &str, checked: &str) -> String {
    match check_plan_of(planned, checked) {
        Ok(()) => panic!("expected the check to reject this plan"),
        Err(error) => error.to_string(),
    }
}

// A `@requires` field set may wrap its body in a type condition that admits everything the
// position holds -- here `... on Review` inside a field of type `[Review]!`. The planner emits the
// entry with that condition flattened away, so the two are compared at different syntax and the
// check must still see them as the same requirement.
#[test]
fn requires_with_a_vacuous_type_condition_is_accepted() {
    check("{ locations { id name reviews { comment rating } } }").unwrap();
}

// The same requirement reached from the other root, so the entity fetch runs under a flatten path
// rather than at the query root.
#[test]
fn requires_reached_through_an_entity_is_accepted() {
    check("{ latestReviews { id location { id name reviews { rating } } } }").unwrap();
}

#[test]
fn entity_fetch_across_subgraphs_is_accepted() {
    check("{ locations { id name overallRating } }").unwrap();
}

// A query plan fetches nothing for introspection, so the checker must not ask it to -- including
// when `__typename` is reached through a named fragment at the root, where it is answered from the
// operation's own root type.
#[test]
fn root_typename_through_a_named_fragment_is_accepted() {
    check("query { ...Root } fragment Root on Query { __typename locations { id } }").unwrap();
}

#[test]
fn root_typename_written_inline_is_accepted() {
    check("{ __typename locations { id } }").unwrap();
}

// Wrapping a selection in a type condition that narrows nothing must not change the verdict: the
// plan is the same plan either way.
#[test]
fn a_vacuous_type_condition_in_the_operation_is_accepted() {
    check("{ locations { ... on Location { id name } } }").unwrap();
}

// One entity fetch over two types whose `@key` field sets name different fields, so the requires
// entries and the entity cases are not interchangeable.
//
// This does *not* reach the branch that reads one type's `@key` at another's entry: the table is
// filled from the pairs of equal type outwards, and on a plan the planner produced those settle
// every row and column, so no other pair is ever computed. Reaching it needs a plan whose diagonal
// fails, which the planner does not emit -- see the note in `mod.rs` on what still has no test.
#[test]
fn entity_cases_with_different_keys_are_accepted() {
    check("{ feed { ... on Book { blurb } ... on Film { summary } } }").unwrap();
}

#[test]
fn a_single_entity_case_is_accepted() {
    check("{ feed { ... on Book { blurb } } }").unwrap();
    check("{ feed { ... on Film { summary } } }").unwrap();
}

// The interface field alongside the entity cases, so the fetch carries a selection that is not
// under any type condition as well as the two that are.
#[test]
fn entity_cases_beside_an_interface_field_are_accepted() {
    check("{ feed { id ... on Book { blurb isbn } ... on Film { summary minutes } } }").unwrap();
}

#[test]
fn a_requires_driven_field_the_plan_never_fetches_is_reported() {
    insta::assert_snapshot!(
        rejection(
            "{ feed { ... on Book { blurb } } }",
            "{ feed { ... on Book { blurb } ... on Film { summary } } }",
        ),
        @r###"
    Correctness error found:
    query plan does not fetch everything the operation requests:
    left does not include right
      in response name: feed
      over runtime types: {Query}
      in sub-selection of: feed -> {Book, Film}
      in response name: summary
      over runtime types: {Film}
      --> left does not select `summary` on Film
    "###
    );
}

#[test]
fn a_field_the_plan_never_fetches_is_reported() {
    insta::assert_snapshot!(
        rejection("{ locations { id } }", "{ locations { id name } }"),
        @r###"
    Correctness error found:
    query plan does not fetch everything the operation requests:
    left does not include right
      in response name: locations
      over runtime types: {Query}
      in sub-selection of: locations -> {Location}
      in response name: name
      over runtime types: {Location}
      --> left does not select `name` on Location
    "###
    );
}

#[test]
fn a_requires_field_the_plan_never_fetches_is_reported() {
    insta::assert_snapshot!(
        rejection("{ locations { id } }", "{ locations { reviews { rating } } }"),
        @r###"
    Correctness error found:
    query plan does not fetch everything the operation requests:
    left does not include right
      in response name: locations
      over runtime types: {Query}
      in sub-selection of: locations -> {Location}
      in response name: reviews
      over runtime types: {Location}
      --> left does not select `reviews` on Location
    "###
    );
}

//==================================================================================================
// Optional checks
//
// These report planner defects the planner cannot currently avoid, so they are off unless asked
// for. No plan the planner produces in these tests carries an empty flatten-path condition, so
// the FED-516 check is pinned on the path directly; the wiring is the one call in the `Flatten`
// arm of `walk_node`.

use apollo_compiler::name;

use super::check_path_reaches_a_type;
use crate::query_plan::FetchDataPathElement;

/// `…|[]` says the planner decided the position admits nothing, so the fetch under the path can
/// never run.
#[test]
fn an_empty_path_type_condition_is_reported() {
    for path in [
        vec![FetchDataPathElement::Key(name!("a"), Some(Vec::new()))],
        vec![FetchDataPathElement::AnyIndex(Some(Vec::new()))],
        vec![
            FetchDataPathElement::Key(name!("a"), None),
            FetchDataPathElement::AnyIndex(Some(Vec::new())),
        ],
    ] {
        let error = check_path_reaches_a_type(&path).expect_err("should be reported");
        assert!(
            error.description().contains("no runtime type"),
            "unexpected message: {error}"
        );
    }
}

/// A condition that names a type, and no condition at all, are both ordinary.
#[test]
fn a_path_that_reaches_a_type_is_not_reported() {
    for path in [
        vec![FetchDataPathElement::Key(name!("a"), None)],
        vec![FetchDataPathElement::Key(
            name!("a"),
            Some(vec![name!("Book")]),
        )],
        vec![FetchDataPathElement::AnyIndex(Some(vec![name!("Book")]))],
        vec![
            FetchDataPathElement::TypenameEquals(name!("Book")),
            FetchDataPathElement::Parent,
        ],
    ] {
        check_path_reaches_a_type(&path).expect("should not be reported");
    }
}
