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
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
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

fn init_operations(planner: &QueryPlanner, node: &mut PlanNode) {
    match node {
        PlanNode::Fetch(fetch) => {
            let schema = planner.subgraph_schemas()[&fetch.subgraph_name].schema();
            fetch
                .operation_document
                .init_parsed(schema)
                .expect("valid subgraph operation");
        }
        PlanNode::Flatten(flatten) => init_operations(planner, &mut flatten.node),
        _ => unimplemented!("not needed by the written plans"),
    }
}

fn written_plan(planner: &QueryPlanner, json: &str) -> QueryPlan {
    let mut node: TopLevelPlanNode = serde_json::from_str(json).expect("valid plan json");
    let TopLevelPlanNode::Sequence(sequence) = &mut node else {
        unimplemented!("not needed by the written plans");
    };
    for node in &mut sequence.nodes {
        init_operations(planner, node);
    }
    QueryPlan {
        node: Some(node),
        statistics: Default::default(),
    }
}

/// A plan whose `reviews` fetch reads `isbn` under an alias and names it back with an input key
/// renamer. `locations_selections` is what the first fetch selects on `Book`, and `aliased` is the
/// field the entry reads under `__require_0_isbn`; varying the two is how these tests separate the
/// key an entry reads from the field it sends.
fn aliased_requires_plan(
    planner: &QueryPlanner,
    locations_selections: &str,
    aliased: &str,
) -> QueryPlan {
    written_plan(
        planner,
        &format!(
            r#"{{ "Sequence": {{ "nodes": [
            {{ "Fetch": {{
                "subgraph_name": "locations",
                "variable_usages": [],
                "operation_document": "{{ feed {{ __typename ... on Book {{ {locations_selections} }} }} }}",
                "operation_kind": "query",
                "input_rewrites": [],
                "output_rewrites": [],
                "context_rewrites": []
            }} }},
            {{ "Flatten": {{
                "path": [{{ "Key": ["feed", null] }}, {{ "AnyIndex": null }}],
                "node": {{ "Fetch": {{
                    "subgraph_name": "reviews",
                    "variable_usages": [],
                    "requires": [{{
                        "kind": "InlineFragment",
                        "typeCondition": "Book",
                        "selections": [
                            {{ "kind": "Field", "name": "__typename" }},
                            {{ "kind": "Field", "name": "id" }},
                            {{ "kind": "Field", "alias": "__require_0_isbn", "name": "{aliased}" }}
                        ]
                    }}],
                    "operation_document": "query($representations: [_Any!]!) {{ _entities(representations: $representations) {{ ... on Book {{ blurb }} }} }}",
                    "operation_kind": "query",
                    "input_rewrites": [{{ "KeyRenamer": {{
                        "path": [{{ "Key": ["__require_0_isbn", null] }}],
                        "rename_key_to": "isbn"
                    }} }}],
                    "output_rewrites": [],
                    "context_rewrites": []
                }} }}
            }} }}
        ] }} }}"#
        ),
    )
}

fn check_blurb_plan(planner: &QueryPlanner, plan: &QueryPlan) -> Result<(), CorrectnessError> {
    let operation = ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ feed { ... on Book { blurb } } }",
        "operation.graphql",
    )
    .expect("valid operation");
    crate::correctness::check_plan(
        planner.api_schema(),
        planner.supergraph_schema(),
        planner.subgraph_schemas(),
        &operation,
        plan,
    )
}

// A planner may alias a `@requires` input, for example when two fetches need the same field with
// different subselections, and rename it back with an input rewrite. The router applies input
// rewrites to each representation before sending it, so `reviews` still receives `isbn`.
#[test]
fn an_aliased_requires_input_renamed_by_an_input_rewrite_is_accepted() {
    let planner = planner();
    let plan = aliased_requires_plan(&planner, "__typename id __require_0_isbn: isbn", "isbn");
    check_blurb_plan(&planner, &plan).unwrap();
}

// The subgraph is sent `isbn`, and the renamer says it is read from `__require_0_isbn`, so that is
// the key the plan has to have fetched. Fetching `isbn` under its own name leaves the rename with
// nothing to carry.
#[test]
fn an_entry_reading_a_key_the_plan_never_fetched_is_rejected() {
    let planner = planner();
    let plan = aliased_requires_plan(&planner, "__typename id isbn", "isbn");
    let error = check_blurb_plan(&planner, &plan).expect_err("`__require_0_isbn` is never fetched");
    assert!(
        error
            .to_string()
            .contains("has not fetched what the subgraph demands"),
        "{error}"
    );
}

// The entry reads `id` under the alias the renamer names `isbn`, so `reviews` would be sent `id`'s
// value as `isbn`. Forwards, the entry as read renames to `isbn` where the entry as sent says
// `id`, and the rewrites and the entry disagree.
#[test]
fn a_renamed_key_naming_a_different_field_is_rejected() {
    let planner = planner();
    let plan = aliased_requires_plan(&planner, "__typename id __require_0_isbn: id", "id");
    let error = check_blurb_plan(&planner, &plan).expect_err("`isbn` would carry `id`'s value");
    assert!(
        error
            .to_string()
            .contains("does not agree with the fetch's input rewrites"),
        "{error}"
    );
}

//==================================================================================================
// An interface object's demand

const INTERFACE_OBJECT: &str = include_str!("testdata/interface_object_requires.graphql");

fn interface_object_planner() -> QueryPlanner {
    let supergraph = Supergraph::new_with_router_specs(INTERFACE_OBJECT).expect("valid supergraph");
    QueryPlanner::new(&supergraph, Default::default()).expect("planner")
}

// `C` declares `I` as an interface object, so its `@requires` on `I.data` is written at `I` and
// the demand derived from it reaches `P` and `Q` both, where a `requires` entry is written at one
// of them. Coverage for that shape, which nothing else here has.
//
// This does not discriminate the fix that asks an entry's inputs only at its own type: the
// planner writes this plan's entry at `I` itself, so restricting the demand to the entry's type
// is the identity. Reaching it needs an entry at an implementation, which is a plan written by
// hand.
#[test]
fn an_interface_object_plan_is_accepted() {
    let planner = interface_object_planner();
    let operation = ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ start { ... on P { data } } }",
        "operation.graphql",
    )
    .expect("valid operation");
    let plan = planner
        .build_query_plan(&operation, None, Default::default())
        .expect("query plan");
    crate::correctness::check_plan(
        planner.api_schema(),
        planner.supergraph_schema(),
        planner.subgraph_schemas(),
        &operation,
        &plan,
    )
    .unwrap();
}

// The subgraph oracle answers in the subgraph's own terms, and `A` declares `I` as an interface
// object — an *object* there, naming itself and nothing else. Ungrounded, that name intersects
// the supergraph's `{P, Q}` to nothing, so every obligation under `start` was dropped and a plan
// that fetches nothing for `data` was accepted. Both checkers share the oracle, so both were
// blind; `SubgraphConstraint` grounds the name back now.
//
// `data` requires `required`, so a plan for `data` fetches both: the operation planned here is
// the smaller one, and `data` is what the plan genuinely lacks.
#[test]
fn a_field_returning_an_interface_object_is_still_checked() {
    let planner = interface_object_planner();
    let planned = ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ start { ... on P { required } } }",
        "planned.graphql",
    )
    .expect("valid operation");
    let plan = planner
        .build_query_plan(&planned, None, Default::default())
        .expect("query plan");
    let checked = ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ start { ... on P { required data } } }",
        "checked.graphql",
    )
    .expect("valid operation");
    let error = crate::correctness::check_plan(
        planner.api_schema(),
        planner.supergraph_schema(),
        planner.subgraph_schemas(),
        &checked,
        &plan,
    )
    .expect_err("the plan never fetches `data`");
    assert!(
        error.to_string().contains("does not select `data`"),
        "{error}"
    );
}
