//! Plan-level tests for the checker.
//!
//! Each case checks a written plan against an operation, with no planner in the loop, so a change
//! to the planner cannot change what is under test. Most plans are in `testdata/plans/`, each one
//! the planner's own plan for an operation, checked in as it was produced; the rest are written
//! inline to reach shapes the planner does not emit. That every plan the planner produces is
//! accepted is the planner tests' concern: `assert_plan!` checks each one.
//!
//! The supergraphs beside this file each say in a header where they come from, so every fixture
//! has a visible source. `entity_requires.graphql` is deliberately close to the shape a corpus
//! graph had when it found a defect here: an entity whose `@requires` field set wraps its body in a
//! type condition that is already the field's own type.

use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;

use super::super::CheckerOptions;
use super::super::CorrectnessError;
use super::ComparisonError;
use super::region::Algebra;
use super::region::Work;
use crate::Supergraph;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;

/// The end of a rejection for a value the plan never fetched where it is read.
const READ_UNFETCHED: &str = "which the plan has not fetched there";

/// The schemas a plan is checked under, read from a supergraph.
struct Fixture {
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    subgraphs: IndexMap<Arc<str>, ValidFederationSchema>,
}

impl Fixture {
    fn new(supergraph: &str) -> Self {
        let supergraph = Supergraph::new_with_router_specs(supergraph).expect("valid fixture");
        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("api schema");
        let subgraphs = supergraph
            .extract_subgraphs()
            .expect("subgraphs")
            .into_iter()
            .map(|(name, subgraph)| (name, subgraph.schema))
            .collect();
        Self {
            supergraph_schema: supergraph.schema.clone(),
            api_schema,
            subgraphs,
        }
    }

    fn operation(&self, source: &str) -> Valid<ExecutableDocument> {
        ExecutableDocument::parse_and_validate(
            self.api_schema.schema(),
            source.to_string(),
            "operation.graphql",
        )
        .expect("valid operation")
    }

    fn check(&self, plan: &QueryPlan, operation: &str) -> Result<(), CorrectnessError> {
        crate::correctness::check_plan(
            &self.api_schema,
            &self.supergraph_schema,
            &self.subgraphs,
            &self.operation(operation),
            plan,
        )
    }

    fn rejection(&self, plan: &QueryPlan, operation: &str) -> String {
        match self.check(plan, operation) {
            Ok(()) => panic!("expected the check to reject this plan"),
            Err(error) => error.to_string(),
        }
    }

    /// [`Self::check`] with the report of a flatten path written to reach no type turned off, so
    /// that what decides is completeness and soundness alone.
    fn check_without_dead_fetch_report(
        &self,
        plan: &QueryPlan,
        operation: &str,
    ) -> Result<(), CorrectnessError> {
        crate::correctness::check_plan_with_options(
            &self.api_schema,
            &self.supergraph_schema,
            &self.subgraphs,
            &self.operation(operation),
            plan,
            CheckerOptions {
                check_empty_flatten_path_type_condition: false,
                ..Default::default()
            },
        )
    }

    /// Parses every fetch's operation against its own subgraph, which a plan read from JSON has
    /// not done yet.
    fn init_operations(&self, node: &mut PlanNode) {
        match node {
            PlanNode::Fetch(fetch) => self.init_operation(fetch),
            PlanNode::Flatten(flatten) => self.init_operations(&mut flatten.node),
            PlanNode::Sequence(sequence) => sequence
                .nodes
                .iter_mut()
                .for_each(|node| self.init_operations(node)),
            PlanNode::Parallel(parallel) => parallel
                .nodes
                .iter_mut()
                .for_each(|node| self.init_operations(node)),
            PlanNode::Defer(defer) => {
                if let Some(node) = &mut defer.primary.node {
                    self.init_operations(node);
                }
                for block in &mut defer.deferred {
                    if let Some(node) = &mut block.node {
                        self.init_operations(node);
                    }
                }
            }
            PlanNode::Condition(condition) => {
                for clause in [&mut condition.if_clause, &mut condition.else_clause]
                    .into_iter()
                    .flatten()
                {
                    self.init_operations(clause);
                }
            }
        }
    }

    fn init_operation(&self, fetch: &mut FetchNode) {
        let schema = self.subgraphs[&fetch.subgraph_name].schema();
        fetch
            .operation_document
            .init_parsed(schema)
            .expect("valid subgraph operation");
    }

    fn written_plan(&self, json: &str) -> QueryPlan {
        let node: TopLevelPlanNode = serde_json::from_str(json).expect("valid plan json");
        let mut node = match node {
            TopLevelPlanNode::Fetch(node) => PlanNode::Fetch(node),
            TopLevelPlanNode::Sequence(node) => PlanNode::Sequence(node),
            TopLevelPlanNode::Parallel(node) => PlanNode::Parallel(node),
            TopLevelPlanNode::Flatten(node) => PlanNode::Flatten(node),
            TopLevelPlanNode::Defer(node) => PlanNode::Defer(node),
            TopLevelPlanNode::Condition(node) => PlanNode::Condition(node),
            TopLevelPlanNode::Subscription(_) => {
                unimplemented!("not needed by the written plans")
            }
        };
        self.init_operations(&mut node);
        let node = match node {
            PlanNode::Fetch(node) => TopLevelPlanNode::Fetch(node),
            PlanNode::Sequence(node) => TopLevelPlanNode::Sequence(node),
            PlanNode::Parallel(node) => TopLevelPlanNode::Parallel(node),
            PlanNode::Flatten(node) => TopLevelPlanNode::Flatten(node),
            PlanNode::Defer(node) => TopLevelPlanNode::Defer(node),
            PlanNode::Condition(node) => TopLevelPlanNode::Condition(node),
        };
        QueryPlan {
            node: Some(node),
            statistics: Default::default(),
        }
    }
}

/// An operation declaring Boolean variables and using each on the root's `__typename`, which the
/// router answers without a fetch and the checker asks nothing of: for written plans whose
/// condition nodes read variables the operation's own selections do not. `operation` is a
/// selection set, `{ … }`.
fn declaring_booleans<V: std::fmt::Display>(
    variables: impl IntoIterator<Item = V>,
    operation: &str,
) -> String {
    let variables: Vec<String> = variables.into_iter().map(|v| v.to_string()).collect();
    let declared: Vec<String> = variables
        .iter()
        .map(|variable| format!("${variable}: Boolean!"))
        .collect();
    let used: String = variables
        .iter()
        .map(|variable| format!("__typename @include(if: ${variable}) "))
        .collect();
    let body = operation
        .trim()
        .strip_prefix('{')
        .and_then(|body| body.strip_suffix('}'))
        .expect("a selection set");
    format!("query({}) {{ {used}{body} }}", declared.join(", "))
}

/// A plan from `testdata/plans/`, by file stem.
macro_rules! plan_json {
    ($name:literal) => {
        include_str!(concat!("testdata/plans/", $name, ".json"))
    };
}

fn fixture() -> Fixture {
    Fixture::new(include_str!("testdata/entity_requires.graphql"))
}

/// Checks a plan from `testdata/plans/` against `operation`.
fn check(plan: &str, operation: &str) -> Result<(), CorrectnessError> {
    let fixture = fixture();
    fixture.check(&fixture.written_plan(plan), operation)
}

fn rejection(plan: &str, operation: &str) -> String {
    let fixture = fixture();
    fixture.rejection(&fixture.written_plan(plan), operation)
}

// A `@requires` field set may wrap its body in a type condition that admits everything the
// position holds -- here `... on Review` inside a field of type `[Review]!`. The planner emits the
// entry with that condition flattened away, so the two are compared at different syntax and the
// check must still see them as the same requirement.
#[test]
fn requires_with_a_vacuous_type_condition_is_accepted() {
    check(
        plan_json!("locations_reviews"),
        "{ locations { id name reviews { comment rating } } }",
    )
    .unwrap();
}

// The same requirement reached from the other root, so the entity fetch runs under a flatten path
// rather than at the query root.
#[test]
fn requires_reached_through_an_entity_is_accepted() {
    check(
        plan_json!("latest_reviews"),
        "{ latestReviews { id location { id name reviews { rating } } } }",
    )
    .unwrap();
}

#[test]
fn entity_fetch_across_subgraphs_is_accepted() {
    check(
        plan_json!("locations_overall_rating"),
        "{ locations { id name overallRating } }",
    )
    .unwrap();
}

// A query plan fetches nothing for introspection, so the checker must not ask it to -- including
// when `__typename` is reached through a named fragment at the root, where it is answered from the
// operation's own root type.
#[test]
fn root_typename_through_a_named_fragment_is_accepted() {
    check(
        plan_json!("locations_id"),
        "query { ...Root } fragment Root on Query { __typename locations { id } }",
    )
    .unwrap();
}

#[test]
fn root_typename_written_inline_is_accepted() {
    check(
        plan_json!("locations_id"),
        "{ __typename locations { id } }",
    )
    .unwrap();
}

// Wrapping a selection in a type condition that narrows nothing must not change the verdict: the
// plan is the same plan either way.
#[test]
fn a_vacuous_type_condition_in_the_operation_is_accepted() {
    check(
        plan_json!("locations_id_name"),
        "{ locations { ... on Location { id name } } }",
    )
    .unwrap();
}

// One entity fetch over two types whose `@key` field sets name different fields: each runtime
// type's representation is read against its own entity type's keys and demand.
#[test]
fn entity_cases_with_different_keys_are_accepted() {
    check(
        plan_json!("feed_blurb_summary"),
        "{ feed { ... on Book { blurb } ... on Film { summary } } }",
    )
    .unwrap();
}

#[test]
fn a_single_entity_case_is_accepted() {
    check(
        plan_json!("feed_blurb"),
        "{ feed { ... on Book { blurb } } }",
    )
    .unwrap();
    check(
        plan_json!("feed_summary"),
        "{ feed { ... on Film { summary } } }",
    )
    .unwrap();
}

// The interface field alongside the two types' fragments, so the fetch carries a selection that is
// not under any type condition as well as the two that are.
#[test]
fn entity_cases_beside_an_interface_field_are_accepted() {
    check(
        plan_json!("feed_beside_interface_field"),
        "{ feed { id ... on Book { blurb isbn } ... on Film { summary minutes } } }",
    )
    .unwrap();
}

#[test]
fn a_requires_driven_field_the_plan_never_fetches_is_reported() {
    insta::assert_snapshot!(
        rejection(
            plan_json!("feed_blurb"),
            "{ feed { ... on Book { blurb } ... on Film { summary } } }",
        ),
        @r###"
    Correctness error found:
    query plan does not fetch everything the operation requests:
      under `feed`
      --> the plan does not select `summary` on `feed`, a `Film`
    "###
    );
}

#[test]
fn a_field_the_plan_never_fetches_is_reported() {
    insta::assert_snapshot!(
        rejection(plan_json!("locations_id"), "{ locations { id name } }"),
        @r###"
    Correctness error found:
    query plan does not fetch everything the operation requests:
      under `locations`
      --> the plan does not select `name` on `locations`, a `Location`
    "###
    );
}

#[test]
fn a_requires_field_the_plan_never_fetches_is_reported() {
    insta::assert_snapshot!(
        rejection(
            plan_json!("locations_id"),
            "{ locations { reviews { rating } } }",
        ),
        @r###"
    Correctness error found:
    query plan does not fetch everything the operation requests:
      under `locations`
      --> the plan does not select `reviews` on `locations`, a `Location`
    "###
    );
}

//==================================================================================================
// Optional checks
//
// These report planner defects the planner cannot currently avoid; both are on by default. No plan
// in these tests carries an empty flatten-path condition, so the FED-516 check is pinned on the
// path directly; the wiring is the one call in the `Flatten` arm of `walk_node`.

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

/// A plan whose `reviews` fetch reads `isbn` under an alias and names it back with an input key
/// renamer. `locations_selections` is what the first fetch selects on `Book`, and `aliased` is the
/// field the entry reads under `__require_0_isbn`; varying the two is how these tests separate the
/// key an entry reads from the field it sends.
fn aliased_requires_plan(
    fixture: &Fixture,
    locations_selections: &str,
    aliased: &str,
) -> QueryPlan {
    fixture.written_plan(
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

fn check_blurb_plan(fixture: &Fixture, plan: &QueryPlan) -> Result<(), CorrectnessError> {
    fixture.check(plan, "{ feed { ... on Book { blurb } } }")
}

// A planner may alias a `@requires` input, for example when two fetches need the same field with
// different subselections, and rename it back with an input rewrite. The router applies input
// rewrites to each representation before sending it, so `reviews` still receives `isbn`.
#[test]
fn an_aliased_requires_input_renamed_by_an_input_rewrite_is_accepted() {
    let fixture = fixture();
    let plan = aliased_requires_plan(&fixture, "__typename id __require_0_isbn: isbn", "isbn");
    check_blurb_plan(&fixture, &plan).unwrap();
}

// The subgraph is sent `isbn`, and the renamer says it is read from `__require_0_isbn`, so that is
// the key the plan has to have fetched. Fetching `isbn` under its own name leaves the rename with
// nothing to carry.
#[test]
fn an_entry_reading_a_key_the_plan_never_fetched_is_rejected() {
    let fixture = fixture();
    let plan = aliased_requires_plan(&fixture, "__typename id isbn", "isbn");
    let error = check_blurb_plan(&fixture, &plan).expect_err("`__require_0_isbn` is never fetched");
    assert!(
        error.to_string().contains(
            "the representation reads `__require_0_isbn` at `feed`, a `Book`, which \
                       the plan has not fetched there"
        ),
        "{error}"
    );
}

// The entry reads `id` under the alias the renamer names `isbn`, so `reviews` would be sent `id`'s
// value as `isbn`. Forwards, the entry as read renames to `isbn` where the entry as sent says
// `id`, and the rewrites and the entry disagree.
#[test]
fn a_renamed_key_naming_a_different_field_is_rejected() {
    let fixture = fixture();
    let plan = aliased_requires_plan(&fixture, "__typename id __require_0_isbn: id", "id");
    let error = check_blurb_plan(&fixture, &plan).expect_err("`isbn` would carry `id`'s value");
    assert!(
        error.to_string().contains(
            "the representation sends `__require_0_isbn` at `feed`, a `Book` as `id`, and the \
             fetch's input rewrites do not carry the one to the other"
        ),
        "{error}"
    );
}

/// A plan whose `reviews` fetch is sent each `Book` of the feed as `entry`, the selections of one
/// `requires` entry on `Book`, under the input rewrites `rewrites`, after `locations` fetched
/// `locations_selections` on each `Book`.
fn book_requires_plan(
    fixture: &Fixture,
    locations_selections: &str,
    entry: serde_json::Value,
    rewrites: serde_json::Value,
) -> QueryPlan {
    let blurb = with(
        entity_fetch("reviews", "Book", entry, "blurb"),
        "input_rewrites",
        rewrites,
    );
    plan_of(
        fixture,
        sequence(vec![
            root_fetch(
                "locations",
                &format!("{{ feed {{ __typename ... on Book {{ {locations_selections} }} }} }}"),
            ),
            flatten(
                serde_json::json!([{ "Key": ["feed", null] }, { "AnyIndex": null }]),
                blurb,
            ),
        ]),
    )
}

/// `__typename`, `id`, and `isbn` read under `__require_0_isbn`, an entry's selections on `Book`.
fn aliased_isbn() -> serde_json::Value {
    serde_json::json!([
        { "kind": "Field", "name": "__typename" },
        { "kind": "Field", "name": "id" },
        { "kind": "Field", "alias": "__require_0_isbn", "name": "isbn" }
    ])
}

/// A key renamer at `path`, with the type filters written before its last key.
fn renaming(path: &[serde_json::Value], to: &str) -> serde_json::Value {
    serde_json::json!([renamer(serde_json::Value::Array(path.to_vec()), to)])
}

fn key(name: &str) -> serde_json::Value {
    serde_json::json!({ "Key": [name, null] })
}

fn typename_equals(type_name: &str) -> serde_json::Value {
    serde_json::json!({ "TypenameEquals": type_name })
}

// The entry reads `isbn` under an alias and nothing names it back: the subgraph would be sent
// `__require_0_isbn`.
//
// The model's `aliasNotRenamedBackDisagrees`. A control of preserved behaviour.
#[test]
fn an_alias_no_rewrite_names_back_is_rejected() {
    let fixture = fixture();
    let plan = book_requires_plan(
        &fixture,
        "__typename id __require_0_isbn: isbn",
        aliased_isbn(),
        serde_json::json!([]),
    );
    let error = check_blurb_plan(&fixture, &plan).expect_err("the alias is sent as it is");
    assert!(
        error.to_string().contains(
            "the representation sends `__require_0_isbn` at `feed`, a `Book` as `isbn`, and the \
             fetch's input rewrites do not carry the one to the other"
        ),
        "{error}"
    );
}

// The alias was fetched, but holding `id`: what the entry reads under it is not `isbn`.
//
// The model's `aliasHoldingAnotherFieldIsNotAvailable`. A control of preserved behaviour.
#[test]
fn an_alias_holding_another_field_is_rejected() {
    let fixture = fixture();
    let plan = aliased_requires_plan(&fixture, "__typename id __require_0_isbn: id", "isbn");
    let error = check_blurb_plan(&fixture, &plan).expect_err("the alias holds `id`");
    assert!(
        error.to_string().contains(
            "the representation reads `__require_0_isbn` at `feed`, a `Book` as `isbn`, where \
             the plan fetched `id` there"
        ),
        "{error}"
    );
}

// The entry reads `isbn` under its own name, which nothing fetched, and the renamer names an alias
// the entry never reads.
//
// The model's `renamerTheEntryDoesNotUseDisagrees`. A control of preserved behaviour.
#[test]
fn a_renamer_the_entry_does_not_use_is_rejected() {
    let fixture = fixture();
    let plan = book_requires_plan(
        &fixture,
        "__typename id __require_0_isbn: isbn",
        entry_fields(&["__typename", "id", "isbn"]),
        renaming(&[key("__require_0_isbn")], "isbn"),
    );
    let error = check_blurb_plan(&fixture, &plan).expect_err("`isbn` is never fetched");
    assert!(
        error
            .to_string()
            .contains("the representation reads `isbn` at `feed`, a `Book`, which"),
        "{error}"
    );
}

// The entry reads `isbn` both under its own name and under the alias, and the renamer sends both
// as `isbn`: inverted, it would read both from the alias.
//
// The model's `twoFieldsSentUnderOneKeyDisagree`. A control of preserved behaviour.
#[test]
fn two_fields_sent_under_one_key_are_rejected() {
    let fixture = fixture();
    let plan = book_requires_plan(
        &fixture,
        "__typename id isbn __require_0_isbn: isbn",
        serde_json::json!([
            { "kind": "Field", "name": "__typename" },
            { "kind": "Field", "name": "id" },
            { "kind": "Field", "name": "isbn" },
            { "kind": "Field", "alias": "__require_0_isbn", "name": "isbn" }
        ]),
        renaming(&[key("__require_0_isbn")], "isbn"),
    );
    let error = check_blurb_plan(&fixture, &plan).expect_err("two values sent as `isbn`");
    assert!(
        error.to_string().contains(
            "the representation sends `isbn` at `feed`, a `Book` as `isbn`, and the fetch's \
             input rewrites do not carry the one to the other"
        ),
        "{error}"
    );
}

// A renamer sends the demanded `isbn` away as `foo`; its demand, its shape and the data are all in
// order, so only the forwards reading catches it.
//
// The model's `renamerSendingAFieldAwayDisagrees`. A control of preserved behaviour.
#[test]
fn a_renamer_sending_a_field_away_is_rejected() {
    let fixture = fixture();
    let plan = book_requires_plan(
        &fixture,
        "__typename id isbn",
        entry_fields(&["__typename", "id", "isbn"]),
        renaming(&[key("isbn")], "foo"),
    );
    let error = check_blurb_plan(&fixture, &plan).expect_err("`isbn` is sent as `foo`");
    assert!(
        error.to_string().contains(
            "the representation sends `isbn` at `feed`, a `Book` as `isbn`, and the fetch's \
             input rewrites do not carry the one to the other"
        ),
        "{error}"
    );
}

/// The aliased `isbn` named back by a renamer filtered on `type_name`.
fn isbn_named_back_on(fixture: &Fixture, type_name: &str) -> QueryPlan {
    book_requires_plan(
        fixture,
        "__typename id __require_0_isbn: isbn",
        aliased_isbn(),
        renaming(
            &[typename_equals(type_name), key("__require_0_isbn")],
            "isbn",
        ),
    )
}

// A renamer filtered on `Book` names the alias back at the `Book`s the fetch is sent: the alias is
// read at each runtime type the copy can be, the filter splitting the configurations by it.
//
// The model's `renamerCoveringPartOfAPositionDisagrees` and
// `renamerNamingBackOnPartOfAPositionDisagrees` split a position the same way, at objects this
// world gives no common field. A control of preserved behaviour.
#[test]
fn a_renamer_filtered_on_the_runtime_type_names_its_alias_back_there() {
    let fixture = fixture();
    check_blurb_plan(&fixture, &isbn_named_back_on(&fixture, "Book")).unwrap();
}

// One filtered on `Film` names it back nowhere a `Book` is.
//
// A control of preserved behaviour.
#[test]
fn a_renamer_filtered_on_another_type_leaves_the_alias_unnamed() {
    let fixture = fixture();
    let error = check_blurb_plan(&fixture, &isbn_named_back_on(&fixture, "Film"))
        .expect_err("nothing names a `Book`'s alias");
    assert!(
        error.to_string().contains(
            "the representation sends `__require_0_isbn` at `feed`, a `Book` as `isbn`, and the \
             fetch's input rewrites do not carry the one to the other"
        ),
        "{error}"
    );
}

//==================================================================================================
// What a deferred block can read

/// A deferred block does not see everything its primary block fetched. The router starts it from
/// what the `defer` node was given and merges in only what the fetches it `depends` on send it,
/// each fetch sending its own response; a block depending on nothing waits for the whole primary
/// response instead.
///
/// The primary block fetches each book's `id` (fetch 0) and then its `isbn` from that `id` (fetch
/// 1), beside the root's `__typename` (fetch 2). The deferred block fetches `blurb`, which
/// requires `isbn` and resolves a `Book` by `id`. `depends` is the deferred block's list.
fn defer_plan(fixture: &Fixture, depends: &[u64]) -> QueryPlan {
    let depends = depends
        .iter()
        .map(|id| format!(r#"{{ "id": "{id}" }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let feed = r#"[{ "Key": ["feed", null] }, { "AnyIndex": null }]"#;
    let entities = |body: &str| {
        format!(
            "query($representations: [_Any!]!) \
             {{ _entities(representations: $representations) {{ ... on Book {{ {body} }} }} }}"
        )
    };
    let book_entry = |fields: &[&str]| {
        let selections = fields
            .iter()
            .map(|name| format!(r#"{{ "kind": "Field", "name": "{name}" }}"#))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"[{{ "kind": "InlineFragment", "typeCondition": "Book", "selections": [{selections}] }}]"#
        )
    };
    let fetch = |subgraph: &str, id: Option<u64>, requires: &str, operation: &str| {
        let id = id.map_or("null".to_string(), |id| id.to_string());
        format!(
            r#"{{ "Fetch": {{
                "subgraph_name": "{subgraph}",
                "id": {id},
                "variable_usages": [],
                "requires": {requires},
                "operation_document": "{operation}",
                "operation_kind": "query",
                "input_rewrites": [],
                "output_rewrites": [],
                "context_rewrites": []
            }} }}"#
        )
    };
    let ids_fetch = fetch(
        "locations",
        Some(0),
        "[]",
        "{ feed { __typename ... on Book { __typename id } } }",
    );
    let isbn_fetch = fetch(
        "locations",
        Some(1),
        &book_entry(&["__typename", "id"]),
        &entities("isbn"),
    );
    let root_typename_fetch = fetch("locations", Some(2), "[]", "{ __typename }");
    let blurb_fetch = fetch(
        "reviews",
        None,
        &book_entry(&["__typename", "id", "isbn"]),
        &entities("blurb"),
    );
    fixture.written_plan(&format!(
        r#"{{ "Defer": {{
                "primary": {{
                    "sub_selection": null,
                    "node": {{ "Parallel": {{ "nodes": [
                        {{ "Sequence": {{ "nodes": [
                            {ids_fetch},
                            {{ "Flatten": {{ "path": {feed}, "node": {isbn_fetch} }} }}
                        ] }} }},
                        {root_typename_fetch}
                    ] }} }}
                }},
                "deferred": [{{
                    "depends": [{depends}],
                    "label": null,
                    "query_path": [],
                    "sub_selection": null,
                    "node": {{ "Flatten": {{ "path": {feed}, "node": {blurb_fetch} }} }}
                }}]
            }} }}"#
    ))
}

fn check_deferred_blurbs(depends: &[u64]) -> Result<(), CorrectnessError> {
    let fixture = fixture();
    let plan = defer_plan(&fixture, depends);
    check_blurb_plan(&fixture, &plan)
}

// FED-1007: the deferred block lists fetch 1 but not fetch 0, which fetch 1 depended on. The
// router hands it fetch 1's response alone, which has each `isbn` but no `id`, so it cannot build
// a representation and `blurb` is never fetched.
#[test]
fn a_deferred_block_missing_an_indirect_dependency_is_rejected() {
    let error = check_deferred_blurbs(&[1]).expect_err("the block cannot read `id`");
    assert!(
        error.to_string().contains("the representation reads")
            && error.to_string().contains(READ_UNFETCHED),
        "{error}"
    );
}

// The block lists only fetch 2, which sends it no feed at all. The deferred fetch is still placed
// at every book the plan fetched, so its requirement is stated there and none of it can be read;
// stated only where the block can reach, it would be no requirement at all.
#[test]
fn a_deferred_block_that_cannot_reach_its_path_is_rejected() {
    check_deferred_blurbs(&[2]).expect_err("the block reads no feed");
}

// Naming both fetches hands the block everything its fetch requires.
#[test]
fn a_deferred_block_naming_every_dependency_is_accepted() {
    check_deferred_blurbs(&[0, 1]).unwrap();
}

// Naming none makes the block wait for the whole primary response.
#[test]
fn a_deferred_block_naming_no_dependency_waits_for_the_primary() {
    check_deferred_blurbs(&[]).unwrap();
}

//==================================================================================================
// An interface object's demand

fn interface_object_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/interface_object_requires.graphql"))
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
    let fixture = interface_object_fixture();
    let plan = fixture.written_plan(plan_json!("interface_object_data"));
    fixture
        .check(&plan, "{ start { ... on P { data } } }")
        .unwrap();
}

// `A` declares `I` as an interface object — an *object* there, naming itself and nothing else.
// What `start` returns from `A` is read in `A`'s terms and grounded in the supergraph, where that
// object stands for each object of the interface `I`, `P` and `Q`. Read as its own name it would
// ground to nothing, every obligation under `start` would be dropped, and a plan that fetches
// nothing for `data` would be accepted.
//
// `data` requires `required`, so the plan for `data` fetches both: the plan here is the planner's
// for `{ start { ... on P { required } } }`, and `data` is what it genuinely lacks.
#[test]
fn a_field_returning_an_interface_object_is_still_checked() {
    let fixture = interface_object_fixture();
    let plan = fixture.written_plan(plan_json!("interface_object_required"));
    let error = fixture
        .check(&plan, "{ start { ... on P { required data } } }")
        .expect_err("the plan never fetches `data`");
    assert!(
        error.to_string().contains("does not select `data`"),
        "{error}"
    );
}

//==================================================================================================
// A contextual value below a union's members

fn union_member_context_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/union_member_context.graphql"))
}

const OUTLETS_PRODUCTS_PRICE: &str =
    "{ outlets { ... on Store { products { price } } ... on Kiosk { products { price } } } }";

const OUTLETS_FEATURED_DELIVERY: &str =
    "{ outlets { ... on Store { featured { delivery } } ... on Kiosk { featured { delivery } } } }";

/// A plan from `testdata/plans/` with the rewrite reading off a kiosk dropped from its entity
/// fetch, the second node of its sequence.
fn without_the_kiosk_rewrite(plan: &str) -> String {
    let mut plan: serde_json::Value = serde_json::from_str(plan).expect("valid plan json");
    let rewrites = plan
        .pointer_mut("/Sequence/nodes/1/Flatten/node/Fetch/context_rewrites")
        .and_then(serde_json::Value::as_array_mut)
        .expect("the entity fetch's context rewrites");
    rewrites.retain(|rewrite| {
        rewrite
            .pointer("/KeyRenamer/path/1/TypenameEquals")
            .and_then(serde_json::Value::as_str)
            != Some("Kiosk")
    });
    assert_eq!(rewrites.len(), 1, "only the store's rewrite is left");
    plan.to_string()
}

// The planner binds `price`'s `locale` with a rewrite per member, each climbing from a product to
// its outlet, so whichever an outlet is, its products are sent its `locale`. Accepted either way:
// on the union the fetch would run nowhere, and on the members each of its positions is covered.
#[test]
fn a_context_rewrite_per_union_member_is_accepted() {
    let fixture = union_member_context_fixture();
    let plan = fixture.written_plan(plan_json!("outlets_products_price"));
    fixture.check(&plan, OUTLETS_PRODUCTS_PRICE).unwrap();
}

// The same with the kiosk's rewrite dropped: a kiosk's products are sent no `locale`. The lookup
// on the member decides this verdict: the fetch runs below `products`, found only on the member
// each fragment names, and on the union it is found nowhere, leaving no position to cover.
#[test]
fn a_union_member_without_a_context_rewrite_is_rejected() {
    let fixture = union_member_context_fixture();
    let plan = fixture.written_plan(&without_the_kiosk_rewrite(plan_json!(
        "outlets_products_price"
    )));
    let error = fixture.rejection(&plan, OUTLETS_PRODUCTS_PRICE);
    assert!(
        error.contains("is not always fetched where the fetch runs"),
        "{error}"
    );
}

// The planner binds `delivery`'s `locale` with a rewrite per member descending past `address`, so
// every featured product is sent its outlet's address's `locale`. The lookup on the member decides
// this verdict, on the value's side: the fetch runs at `featured`, which ends its path and is found
// without one, but `address` is found only on the member, and on the union no value is found. A
// fetch below a member's own field cannot show this, its position being found nowhere on the
// union either.
#[test]
fn a_context_value_below_a_union_members_field_is_accepted() {
    let fixture = union_member_context_fixture();
    let plan = fixture.written_plan(plan_json!("outlets_featured_delivery"));
    fixture.check(&plan, OUTLETS_FEATURED_DELIVERY).unwrap();
}

// The same with the kiosk's rewrite dropped: a kiosk's featured product is sent no `locale`, the
// store's address serving no kiosk, since the member's narrowing is carried past `address`.
// Rejected either way, with no value on the union and only the store's on the members.
#[test]
fn a_context_value_below_another_union_members_field_is_rejected() {
    let fixture = union_member_context_fixture();
    let plan = fixture.written_plan(&without_the_kiosk_rewrite(plan_json!(
        "outlets_featured_delivery"
    )));
    let error = fixture.rejection(&plan, OUTLETS_FEATURED_DELIVERY);
    assert!(
        error.contains("is not always fetched where the fetch runs"),
        "{error}"
    );
}

//==================================================================================================
// A contextual value under a fragment that states its path condition

fn owner_profile_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/owner_profile_context.graphql"))
}

const STORES_PRODUCTS_PRICE: &str = "{ stores { products { price } } }";

/// The planner's plan, with the profile's `locale` fetched once for the owner types `owners`
/// names, at `stores.@.owner|[…].profile`, as a planner serving several of an interface's
/// implementations with one entity fetch writes the condition, and the root fetch selecting the
/// owner's `profile` inside `... on Owner`, which reaches exactly `Person` and `Company`.
fn owner_profile_plan(fixture: &Fixture, owners: &[&str]) -> QueryPlan {
    let mut plan: serde_json::Value =
        serde_json::from_str(plan_json!("stores_products_price")).expect("valid plan json");
    let root = plan
        .pointer_mut("/Sequence/nodes/0/Fetch/operation_document")
        .expect("the root fetch's operation");
    *root = serde_json::json!(
        "{ stores { __typename owner { __typename ... on Owner { profile { __typename id } } } \
         products { __typename id } } }"
    );
    let owner = plan
        .pointer_mut("/Sequence/nodes/1/Flatten/path/2/Key/1")
        .expect("the condition on the locale fetch's `owner`");
    *owner = serde_json::json!(owners);
    fixture.written_plan(&plan.to_string())
}

// The `locale` is fetched for both types an owner can have, below the root fetch's fragment on
// `Owner`, which states the path's condition: whichever object a store's owner is, its profile
// carries a `locale`, past the store, the last level its path shares with the fetch for `price`,
// so every product is sent its owner's `locale`.
//
// A control: a check reading each owner's copy alone, rather than asking every object the owner
// can be, rejects the plan though the value was fetched for every owner. The model's
// `sharedOwnerContext`.
#[test]
fn a_context_value_under_a_fragment_stating_its_condition_is_accepted() {
    let fixture = owner_profile_fixture();
    let plan = owner_profile_plan(&fixture, &["Person", "Company"]);
    fixture.check(&plan, STORES_PRODUCTS_PRICE).unwrap();
}

// The `locale` fetched only where the owner is a `Person`: a company's products are sent none.
//
// A control of preserved behaviour: the root fetch's fragment on `Owner` reaches `Company` too,
// but the locale fetch's path names `Person` alone, so a company's profile is never given one.
// The model's `sharedOwnerMissingContext`.
#[test]
fn a_context_value_missing_for_one_owner_type_is_rejected() {
    let fixture = owner_profile_fixture();
    let plan = owner_profile_plan(&fixture, &["Person"]);
    let error = fixture.rejection(&plan, STORES_PRODUCTS_PRICE);
    assert!(
        error.contains("is not always fetched where the fetch runs"),
        "{error}"
    );
}

//==================================================================================================
// A path condition's names, compared as the router compares them
//
// The router selects the objects a conditioned path element reaches by comparing each name of the
// condition literally with the object's `__typename` (`iterate_path` in the router's
// `json_ext.rs`), so a name that is no object type, an interface's or a union's, selects nothing,
// and a fetch below a condition naming no object of its position never runs. Each plan here is
// the planner's, with a condition written on the path of the only fetch supplying a field, or on
// the path a contextual value is read through. The report of a flatten path written to reach no
// type is off: it reads only whether the written list is empty, and none here is.

fn condition_names_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/condition_names.graphql"))
}

/// The planner's plan `plan` with the type condition at the JSON pointer `condition` set to
/// `names`.
fn with_condition(fixture: &Fixture, plan: &str, condition: &str, names: &[&str]) -> QueryPlan {
    let mut plan: serde_json::Value = serde_json::from_str(plan).expect("valid plan json");
    *plan
        .pointer_mut(condition)
        .expect("a type condition at the pointer") = serde_json::json!(names);
    fixture.written_plan(&plan.to_string())
}

/// The three places a condition is written, with the operation each plan is for and the field
/// only the conditioned fetch supplies: on a key with more path below it
/// (`featured|….author`), on a key ending the path (`featured|…`), and on an index ending it
/// (`items.@|…`).
const CONDITION_PLACEMENTS: [(&str, &str, &str, &str); 3] = [
    (
        plan_json!("featured_author_name"),
        "/Sequence/nodes/1/Flatten/path/0/Key/1",
        "{ featured { author { name } } }",
        "`name`",
    ),
    (
        plan_json!("featured_blurb"),
        "/Sequence/nodes/1/Flatten/path/0/Key/1",
        "{ featured { ... on Book { blurb } ... on Film { blurb } } }",
        "`blurb`",
    ),
    (
        plan_json!("items_blurb"),
        "/Sequence/nodes/1/Flatten/path/1/AnyIndex",
        "{ items { ... on Book { blurb } ... on Film { blurb } } }",
        "`blurb`",
    ),
];

// A condition naming the interface `Item`, in each of the three places. No item's `__typename` is
// `Item`, so the fetch under it runs for none of them and the plan never fetches what the client
// asks.
//
// A control: a check reading the names as a fragment's type condition, which grounds `Item` to
// every item, accepts the plans below a key with more path, and one not reading the condition at
// the end of a path accepts the others.
#[test]
fn a_fetch_under_an_interface_name_is_rejected() {
    let fixture = condition_names_fixture();
    for (plan, condition, operation, field) in CONDITION_PLACEMENTS {
        let plan = with_condition(&fixture, plan, condition, &["Item"]);
        let error = fixture
            .check_without_dead_fetch_report(&plan, operation)
            .expect_err("the fetch under `Item` never runs");
        assert!(
            error
                .to_string()
                .contains(&format!("does not select {field}")),
            "{condition}: {error}"
        );
    }
}

// An empty condition ending the path, `featured|[]` and `items.@|[]`: it names no object, so the
// fetch under it runs for no item, and the plan never fetches the `blurb`s.
//
// A control of changed behaviour: the end of a path did not read its condition at all, and with
// the report of an empty condition off each plan was accepted.
#[test]
fn a_fetch_at_an_empty_condition_ending_its_path_is_rejected() {
    let fixture = condition_names_fixture();
    for (plan, condition, operation, field) in &CONDITION_PLACEMENTS[1..] {
        let plan = with_condition(&fixture, plan, condition, &[]);
        let error = fixture
            .check_without_dead_fetch_report(&plan, operation)
            .expect_err("the fetch under `[]` never runs");
        assert!(
            error
                .to_string()
                .contains(&format!("does not select {field}")),
            "{condition}: {error}"
        );
    }
}

// A condition naming `Item`, or naming nothing, on the key before an unrestricted index,
// `items|[Item].@` and `items|[].@`: the index consumes no response key, so the condition still
// holds of the items below it, none of which it selects, and the plan never fetches their
// `blurb`. The report of an empty condition, on, reports the second and not the first.
//
// A control of changed behaviour: the index replaced the condition carried to it with its own,
// none, and the fetch was credited as if unconditioned.
#[test]
fn a_fetch_under_a_key_condition_before_an_index_is_rejected() {
    let fixture = condition_names_fixture();
    let (plan, _, operation, _) = CONDITION_PLACEMENTS[2];
    let on_key = "/Sequence/nodes/1/Flatten/path/0/Key/1";
    let item = with_condition(&fixture, plan, on_key, &["Item"]);
    for result in [
        fixture.check_without_dead_fetch_report(&item, operation),
        fixture.check(&item, operation),
    ] {
        let error = result.expect_err("the fetch under `items|[Item]` never runs");
        assert!(
            error.to_string().contains("does not select `blurb`"),
            "{error}"
        );
    }
    let empty = with_condition(&fixture, plan, on_key, &[]);
    let error = fixture
        .check_without_dead_fetch_report(&empty, operation)
        .expect_err("the fetch under `items|[]` never runs");
    assert!(
        error.to_string().contains("does not select `blurb`"),
        "{error}"
    );
}

// The same three conditions naming the interface's members, as a planner writes them: every item
// is one of them, and each plan is complete and sound.
//
// A control of preserved behaviour.
#[test]
fn a_fetch_under_member_names_is_accepted() {
    let fixture = condition_names_fixture();
    for (plan, condition, operation, _) in CONDITION_PLACEMENTS {
        let plan = with_condition(&fixture, plan, condition, &["Book", "Film"]);
        fixture
            .check_without_dead_fetch_report(&plan, operation)
            .unwrap_or_else(|error| panic!("{condition}: {error}"));
    }
}

// `Person` is an object type, but no featured item is one, so the fetch below
// `featured|[Person]` runs nowhere.
//
// A control of preserved behaviour at the coverage boundary: the fetch runs at no object, so the
// author's `name` it selects is added nowhere, and the client's `name` is not fetched.
#[test]
fn a_fetch_under_an_object_type_the_position_cannot_hold_is_rejected() {
    let fixture = condition_names_fixture();
    let (plan, condition, operation, _) = CONDITION_PLACEMENTS[0];
    let plan = with_condition(&fixture, plan, condition, &["Person"]);
    let error = fixture
        .check_without_dead_fetch_report(&plan, operation)
        .expect_err("the fetch under `Person` never runs");
    assert!(
        error.to_string().contains("does not select `name`"),
        "{error}"
    );
}

const STORES_PRODUCTS_LABEL_PRICE: &str = "{ stores { products { price } } }";

/// The planner's plan for every product's `price`, its `label` read off its store's owner through
/// `owner|owners`: the condition is on the value's own path past the store, the last level it
/// shares with the fetch, and the fetch's own path carries none.
fn price_through(fixture: &Fixture, owners: &[&str]) -> QueryPlan {
    with_condition(
        fixture,
        plan_json!("stores_products_label_price"),
        "/Sequence/nodes/1/Flatten/node/Fetch/context_rewrites/0/KeyRenamer/path/2/Key/1",
        owners,
    )
}

// The `label` is fetched for every owner, but read through `owner|[Owner]`, which no owner's
// `__typename` matches: the router binds no value, and every price is asked with none. Everything
// else the plan needs is there, as the next control shows, so this is the contextual value's
// availability alone.
//
// A control of changed behaviour: read as the types it grounds to, the name left the owner
// unnarrowed and the value was found.
#[test]
fn a_context_value_read_through_an_interface_name_is_rejected() {
    let fixture = condition_names_fixture();
    let plan = price_through(&fixture, &["Owner"]);
    let error = fixture
        .check_without_dead_fetch_report(&plan, STORES_PRODUCTS_LABEL_PRICE)
        .expect_err("no owner's `__typename` is `Owner`");
    assert!(
        error
            .to_string()
            .contains("is not always fetched where the fetch runs"),
        "{error}"
    );
}

// The same read through `owner|[Person,Company]`, which every owner matches.
//
// A control of preserved behaviour.
#[test]
fn a_context_value_read_through_member_names_is_accepted() {
    let fixture = condition_names_fixture();
    let plan = price_through(&fixture, &["Person", "Company"]);
    fixture
        .check_without_dead_fetch_report(&plan, STORES_PRODUCTS_LABEL_PRICE)
        .unwrap();
}

//==================================================================================================
// What a fetch's subgraph can resolve
//
// A subgraph answers a fetch only for the fields it can resolve: a field it marks `@external` is
// resolved by another subgraph, and by this one only inside a `@provides` window of its own that
// names it where the fetch selects it. Every fetch is checked, root fetches included. Each test
// below is the port of a theorem of the model's `Resolution` tests, named in its comment, over the
// world `subgraph_resolution.graphql` describes: an accepted plan is the planner's, a rejected one
// is written here, and each fetches what its operation asks for, so that what the subgraph can
// resolve alone decides.
//
// The model's `fieldTheSubgraphDoesNotDeclareIsRejected` and
// `rootFetchToAnUnknownSubgraphIsRejected` have no counterpart here: `Fixture` validates every
// fetch's operation against its subgraph, and reads no plan naming a subgraph it does not have.

use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;

fn resolution_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/subgraph_resolution.graphql"))
}

/// A plan of one fetch, asking `subgraph` for `operation`.
fn fetch_plan(fixture: &Fixture, subgraph: &str, operation: &str) -> QueryPlan {
    fixture.written_plan(
        &serde_json::json!({ "Fetch": {
            "subgraph_name": subgraph,
            "variable_usages": [],
            "operation_document": operation,
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": []
        } })
        .to_string(),
    )
}

/// Each product's `field` with its key from `catalog`, then `shop` asked, under `_entities`, for
/// `selection` of each, an `entity_type` it resolves by `id`.
fn shop_entity_plan(
    fixture: &Fixture,
    field: &str,
    entity_type: &str,
    selection: &str,
) -> QueryPlan {
    let representation = serde_json::json!([
        { "kind": "Field", "name": "__typename" },
        { "kind": "Field", "name": "id" }
    ]);
    let entities = format!(
        "query($representations: [_Any!]!) {{ _entities(representations: $representations) \
         {{ ... on {entity_type} {{ {selection} }} }} }}"
    );
    fixture.written_plan(
        &serde_json::json!({ "Sequence": { "nodes": [
            { "Fetch": {
                "subgraph_name": "catalog",
                "variable_usages": [],
                "operation_document": format!("{{ products {{ {field} {{ __typename id }} }} }}"),
                "operation_kind": "query",
                "input_rewrites": [],
                "output_rewrites": [],
                "context_rewrites": []
            } },
            { "Flatten": {
                "path": [
                    { "Key": ["products", null] },
                    { "AnyIndex": null },
                    { "Key": [field, null] }
                ],
                "node": { "Fetch": {
                    "subgraph_name": "shop",
                    "variable_usages": [],
                    "requires": [{
                        "kind": "InlineFragment",
                        "typeCondition": entity_type,
                        "selections": representation
                    }],
                    "operation_document": entities,
                    "operation_kind": "query",
                    "input_rewrites": [],
                    "output_rewrites": [],
                    "context_rewrites": []
                } }
            } }
        ] } })
        .to_string(),
    )
}

/// The rejection of a plan asking `shop` for `field`, external there, where no window grants it.
fn assert_not_resolved(fixture: &Fixture, plan: &QueryPlan, operation: &str, field: &str) {
    let error = fixture.rejection(plan, operation);
    assert!(
        error.contains(&format!(
            "fetch to shop: `{field}` is external in the subgraph, and no `@provides` window"
        )),
        "{error}"
    );
}

const TOP_PRODUCTS_VENDOR_NAME: &str = "{ topProducts { vendor { name } } }";

// `topProducts` opens a window naming `vendor { name }`, so `shop` can resolve `Vendor.name`,
// external there, at the end of that path. The model's `externalFieldInsideItsWindowIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn an_external_field_inside_its_window_is_accepted() {
    let fixture = resolution_fixture();
    let plan = fixture.written_plan(plan_json!("top_products_vendor_name"));
    fixture.check(&plan, TOP_PRODUCTS_VENDOR_NAME).unwrap();
}

// The window grants `name` under `vendor` alone, so under its sibling `maker` `shop` is asked for
// `Vendor.name` where it cannot resolve it. The model's `externalFieldOnASiblingPathIsRejected`.
//
// A control of changed behaviour: the plan's buffer holds `name` either way, and nothing read the
// subgraph's declarations.
#[test]
fn an_external_field_on_a_sibling_path_is_rejected() {
    let fixture = resolution_fixture();
    let operation = "{ topProducts { maker { name } } }";
    let plan = fetch_plan(&fixture, "shop", operation);
    assert_not_resolved(&fixture, &plan, operation, "Vendor.name");
}

const PRODUCTS_VENDOR_NAME: &str = "{ products { vendor { name } } }";

// `products` opens no window, so `shop` can resolve `Vendor.name` nowhere below it, and a root
// fetch is held to what its subgraph can resolve like any other. The model's
// `externalFieldOutsideAWindowIsRejected`.
//
// A control of changed behaviour.
#[test]
fn an_external_field_outside_a_window_is_rejected() {
    let fixture = resolution_fixture();
    let plan = fetch_plan(&fixture, "shop", PRODUCTS_VENDOR_NAME);
    assert_not_resolved(&fixture, &plan, PRODUCTS_VENDOR_NAME, "Vendor.name");
}

// The same operation asked of `catalog`, whose metadata says it can resolve `Vendor.name`: what
// the subgraph can resolve decides, and a window is needed only for a field it cannot. The
// planner's plan. The model's `theSameFieldResolvedOutrightIsAccepted`, which varies the metadata
// of one subgraph where this world has the two.
//
// A control of preserved behaviour.
#[test]
fn the_same_field_resolved_outright_is_accepted() {
    let fixture = resolution_fixture();
    let plan = fixture.written_plan(plan_json!("products_vendor_name"));
    fixture.check(&plan, PRODUCTS_VENDOR_NAME).unwrap();
}

// `Product.vendor` opens a window granting `code` inside the one `topProducts` opens, and the two
// add up: `shop` can resolve both external fields under `vendor`. The model's
// `nestedWindowAddsToTheOuterOneIsAccepted`.
//
// A control of preserved behaviour, which a window replacing the one it opens inside would fail.
#[test]
fn a_nested_window_adding_to_the_outer_one_is_accepted() {
    let fixture = resolution_fixture();
    let plan = fixture.written_plan(plan_json!("top_products_vendor_name_code"));
    fixture
        .check(&plan, "{ topProducts { vendor { name code } } }")
        .unwrap();
}

// `featured`'s window grants `maker { name }` under `... on Gizmo`, where `shop` is asked for it.
// The model's `conditionedWindowGrantsItsTypeIsAccepted`; the axes case `inside-the-condition`.
//
// A control of preserved behaviour.
#[test]
fn a_conditioned_window_granting_its_type_is_accepted() {
    let fixture = resolution_fixture();
    let plan = fixture.written_plan(plan_json!("featured_gizmo_maker_name"));
    fixture
        .check(&plan, "{ featured { ... on Gizmo { maker { name } } } }")
        .unwrap();
}

// The window's condition admits `Gizmo` alone, so under `... on Widget` the maker's `name` is not
// granted. The model's `conditionedWindowDoesNotGrantAnotherMemberIsRejected`; the axes case
// `outside-the-condition`.
//
// A control of changed behaviour.
#[test]
fn a_conditioned_window_not_granting_another_member_is_rejected() {
    let fixture = resolution_fixture();
    let plan = fetch_plan(
        &fixture,
        "shop",
        "{ featured { __typename ... on Widget { maker { name } } } }",
    );
    assert_not_resolved(
        &fixture,
        &plan,
        "{ featured { ... on Widget { maker { name } } } }",
        "Vendor.name",
    );
}

// `maker` spelled on `Item` reaches a widget's maker too, which the window does not grant: the
// members carry different windows and are asked one at a time, which the two fragments tested
// apart do not show. The model's `interfaceFieldReachingAnUngrantedMemberIsRejected`; the axes case
// `across-the-members`.
//
// A control of changed behaviour.
#[test]
fn an_interface_field_reaching_an_ungranted_member_is_rejected() {
    let fixture = resolution_fixture();
    let plan = fetch_plan(
        &fixture,
        "shop",
        "{ featured { __typename maker { name } } }",
    );
    assert_not_resolved(
        &fixture,
        &plan,
        "{ featured { maker { name } } }",
        "Vendor.name",
    );
}

// `label` spelled on `Item` is asked of every member `items` can hold, and `shop` marks
// `Gizmo.label` external with no window open. The model's
// `interfaceFieldReachingAnExternalMemberIsRejected`.
//
// A control of changed behaviour.
#[test]
fn an_interface_field_reaching_an_external_member_is_rejected() {
    let fixture = resolution_fixture();
    let operation = "{ items { label } }";
    let plan = fetch_plan(&fixture, "shop", operation);
    assert_not_resolved(&fixture, &plan, operation, "Gizmo.label");
}

// `shop` resolves `Widget.label` itself, and the fragment reaches no gizmo. The planner's plan. The
// model's `memberResolvingTheFieldIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn a_member_resolving_the_field_is_accepted() {
    let fixture = resolution_fixture();
    let plan = fixture.written_plan(plan_json!("items_widget_label"));
    fixture
        .check(&plan, "{ items { ... on Widget { label } } }")
        .unwrap();
}

// Selecting the key the fetch is keyed by asks `shop` to resolve it, which it does: `Vendor.id` is
// marked external on the type extension its key is on, a fake external, which the metadata reads
// as one it can resolve. The model's `keySelectedUnderEntitiesIsAccepted`.
//
// A control of preserved behaviour, which a reading of `@external` that missed the fake would fail.
#[test]
fn a_key_selected_under_entities_is_accepted() {
    let fixture = resolution_fixture();
    let plan = shop_entity_plan(&fixture, "vendor", "Vendor", "id");
    fixture
        .check(&plan, "{ products { vendor { id } } }")
        .unwrap();
}

// `shop` marks `Brand` external as a type, its key field `id` included, so the same fetch for a
// brand asks `shop` for a field it cannot resolve: a representation's members travel in the
// variable, and selecting one of them is no exemption. The model's
// `externalKeySelectedUnderEntitiesIsRejected`.
//
// A control of changed behaviour.
#[test]
fn an_external_key_selected_under_entities_is_rejected() {
    let fixture = resolution_fixture();
    let plan = shop_entity_plan(&fixture, "brand", "Brand", "id");
    assert_not_resolved(&fixture, &plan, "{ products { brand { id } } }", "Brand.id");
}

// `shop` can resolve `__typename` on a type it can resolve none of the fields of, with no
// declaration. The model's `typenameIsResolvableWithoutADeclaration`, which the model asks of its
// walk alone and the whole checker reaches here.
//
// A control of preserved behaviour.
#[test]
fn typename_is_resolvable_without_a_declaration() {
    let fixture = resolution_fixture();
    let plan = shop_entity_plan(&fixture, "brand", "Brand", "__typename");
    fixture
        .check(&plan, "{ products { brand { __typename } } }")
        .unwrap();
}

// What the checker reads in `shop` as extracted from the supergraph, whose `Vendor.id` keeps its
// marking by a hand edit: the key field of the type extension is a fake external, which it can
// resolve; `Brand.id` is external through its type, and `Vendor.name` external.
#[test]
fn the_extracted_shop_reads_its_externals_as_written() {
    let fixture = resolution_fixture();
    let metadata = fixture.subgraphs["shop"]
        .subgraph_metadata()
        .expect("subgraph metadata");
    let field = |type_name, field_name| {
        FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
            type_name,
            field_name,
        })
    };
    let vendor_id = field(name!("Vendor"), name!("id"));
    assert!(metadata.is_field_fake_external(&vendor_id));
    assert!(!metadata.is_field_external(&vendor_id));
    assert!(metadata.is_field_external(&field(name!("Brand"), name!("id"))));
    assert!(metadata.is_field_external(&field(name!("Vendor"), name!("name"))));
}

//==================================================================================================
// Contextual arguments: bound, and read from their declaration
//
// A field with a `@fromContext` argument is resolved with a value the router reads from above it
// and passes as a variable a context rewrite binds. Every such argument a fetch's operation passes
// has to be passed a variable one of its rewrites binds, and every rewrite binding one has to read,
// through what was fetched, a leaf of the declaration off an object setting the context. Each test
// below is the port of a theorem of the model's `ContextArguments` tests, named in its comment,
// over the world `context_arguments.graphql` describes: an accepted plan is the planner's, and a
// rejected one the planner's edited as its comment says.
//
// A rewrite climbing past the nearest object setting the context to another that sets it too
// reads a value of the declared shape from the wrong object, and is accepted: which setter is the
// nearest is not asked, and no test here asserts that verdict.

fn context_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/context_arguments.graphql"))
}

/// Where the planner's plans for the context world keep the first fetch's operation, from
/// `directory`, and the second fetch, to `stores`.
const DIRECTORY_OPERATION: &str = "/Sequence/nodes/0/Fetch/operation_document";
const STORES_FETCH: &str = "/Sequence/nodes/1/Flatten/node/Fetch";

/// A plan from `testdata/plans/`, edited as JSON before it is read.
fn edited_plan(
    fixture: &Fixture,
    plan: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> QueryPlan {
    let mut plan: serde_json::Value = serde_json::from_str(plan).expect("valid plan json");
    edit(&mut plan);
    fixture.written_plan(&plan.to_string())
}

/// Replaces the one occurrence of `from` in the string at the JSON pointer `at`.
fn replace_once(plan: &mut serde_json::Value, at: &str, from: &str, to: &str) {
    let value = plan.pointer_mut(at).expect("a value at the pointer");
    let text = value.as_str().expect("a string at the pointer");
    assert_eq!(text.matches(from).count(), 1, "`{from}` in `{text}`");
    *value = serde_json::Value::String(text.replacen(from, to, 1));
}

/// Sets the value at the JSON pointer `at`.
fn set(plan: &mut serde_json::Value, at: &str, value: serde_json::Value) {
    *plan.pointer_mut(at).expect("a value at the pointer") = value;
}

/// The path of the stores fetch's context rewrite `index`.
fn rewrite_path(index: usize) -> String {
    format!("{STORES_FETCH}/context_rewrites/{index}/KeyRenamer/path")
}

/// A rewrite path climbing `climb` levels to an object of `type_name` and reading its `key`.
fn reading(climb: usize, type_name: &str, key: &str) -> serde_json::Value {
    let mut path = vec![serde_json::json!("Parent"); climb];
    path.push(serde_json::json!({ "TypenameEquals": type_name }));
    path.push(serde_json::json!({ "Key": [key, null] }));
    serde_json::Value::Array(path)
}

fn assert_rejected_for(fixture: &Fixture, plan: &QueryPlan, operation: &str, clause: &str) {
    let error = fixture.rejection(plan, operation);
    assert!(error.contains(clause), "{error}");
}

const STORE_OFFERS_LABEL: &str = "{ regions { stores { offers { label } } } }";

/// The binding clause of `Offer.label`'s `locale`.
const LABEL_LOCALE: &str = "`Offer.label` takes `locale` from context `locality`, and the fetch";

const NOT_FETCHED: &str = "is not always fetched where the fetch runs";

const NOT_A_LEAF: &str = "not a leaf of the declaration's selection there";

// `label` is passed a variable a rewrite binds to `locale` read off the store one level up, an
// object setting `locality`. The planner's plan. The model's
// `contextualArgumentBoundAndReadIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn a_contextual_argument_bound_and_read_is_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("regions_stores_offers_label"));
    fixture.check(&plan, STORE_OFFERS_LABEL).unwrap();
}

/// The planner's plan for `label` below each store with the stores fetch's `label` passed
/// `argument`, its variable undeclared, and no context rewrite.
fn label_passed(fixture: &Fixture, argument: &str) -> QueryPlan {
    edited_plan(fixture, plan_json!("regions_stores_offers_label"), |plan| {
        let operation = format!("{STORES_FETCH}/operation_document");
        replace_once(
            plan,
            &operation,
            "label(locale: $contextualArgument_2_0)",
            argument,
        );
        replace_once(plan, &operation, ", $contextualArgument_2_0: String", "");
        set(
            plan,
            &format!("{STORES_FETCH}/variable_usages"),
            serde_json::json!([]),
        );
        set(
            plan,
            &format!("{STORES_FETCH}/context_rewrites"),
            serde_json::json!([]),
        );
    })
}

// `label` is selected with no `locale` and the fetch declares no rewrite, so `stores` resolves it
// with no value where its declaration takes the store's. Nothing but the binding obligation sees
// this: the fetch declares no rewrite to be checked, and still fetches the `label` asked for. The
// model's `argumentAndRewriteBothOmittedIsRejected`.
//
// A control of changed behaviour.
#[test]
fn an_argument_and_its_rewrite_both_omitted_are_rejected() {
    let fixture = context_fixture();
    let plan = label_passed(&fixture, "label");
    assert_rejected_for(
        &fixture,
        &plan,
        STORE_OFFERS_LABEL,
        &format!("{LABEL_LOCALE} passes it no variable a context rewrite binds"),
    );
}

// `label` is passed a variable no rewrite binds, which reaches `stores` with no value. The model's
// `argumentPassedWithNoRewriteIsRejected`.
//
// The verdict is preserved and the clause asserted changed: before, completeness rejected the plan,
// reading the argument as the client's. The model satisfies the client's demand from another
// subgraph so that binding alone rejects; no other subgraph here resolves `label`, and the clause
// is what holds the binding obligation responsible.
#[test]
fn an_argument_passed_with_no_rewrite_is_rejected() {
    let fixture = context_fixture();
    let plan = edited_plan(
        &fixture,
        plan_json!("regions_stores_offers_label"),
        |plan| {
            set(
                plan,
                &format!("{STORES_FETCH}/context_rewrites"),
                serde_json::json!([]),
            );
        },
    );
    assert_rejected_for(
        &fixture,
        &plan,
        STORE_OFFERS_LABEL,
        &format!(
            "{LABEL_LOCALE} passes it `$contextualArgument_2_0`, which none of its context \
             rewrites binds"
        ),
    );
}

// `label` is passed a literal where its declaration takes the context's value: a literal is no
// variable a rewrite binds. The model's `argumentPassedALiteralIsRejected`.
//
// The verdict is preserved and the clause asserted changed, as for the variable no rewrite binds.
#[test]
fn an_argument_passed_a_literal_is_rejected() {
    let fixture = context_fixture();
    let plan = label_passed(&fixture, "label(locale: \"en\")");
    assert_rejected_for(
        &fixture,
        &plan,
        STORE_OFFERS_LABEL,
        &format!("{LABEL_LOCALE} passes it no variable a context rewrite binds"),
    );
}

/// The planner's plan for `label` below each store with `directory`'s selection of the store's
/// `locale` written as `store_locale` and the rewrite's path as `rewrite`.
fn label_reading(fixture: &Fixture, store_locale: &str, rewrite: serde_json::Value) -> QueryPlan {
    edited_plan(fixture, plan_json!("regions_stores_offers_label"), |plan| {
        replace_once(
            plan,
            DIRECTORY_OPERATION,
            "stores { __typename locale offers",
            &format!("stores {{ __typename {store_locale} offers"),
        );
        set(plan, &rewrite_path(0), rewrite);
    })
}

// The rewrite climbs to the store, which sets `locality`, but reads its `id` where the declaration
// selects `{ locale }`. The `id` is fetched, so only the declaration tells. The model's
// `rewriteReadingAnotherLeafIsRejected`.
//
// A control of changed behaviour.
#[test]
fn a_rewrite_reading_another_leaf_is_rejected() {
    let fixture = context_fixture();
    let plan = label_reading(&fixture, "id locale", reading(1, "Store", "id"));
    assert_rejected_for(&fixture, &plan, STORE_OFFERS_LABEL, NOT_A_LEAF);
}

// `directory` put the store's `currency` under the response key `locale`, and the rewrite reads
// that key: the router forwards the currency, whatever the key is called, so it is the field under
// the key that is compared with the declaration. The model's `aliasedWrongFieldIsRejected`.
//
// A control of changed behaviour.
#[test]
fn an_aliased_wrong_field_is_rejected() {
    let fixture = context_fixture();
    let plan = label_reading(&fixture, "locale: currency", reading(1, "Store", "locale"));
    assert_rejected_for(
        &fixture,
        &plan,
        STORE_OFFERS_LABEL,
        &format!("on `Store` holds `currency`, {NOT_A_LEAF}"),
    );
}

// `directory` put the store's `locale` under the response key `ctxLocale`, and the rewrite reads
// that key, under which the declared field sits. The model's `aliasedDeclaredFieldIsAccepted`.
//
// A control of preserved behaviour, which a reading comparing the key's spelling would fail.
#[test]
fn an_aliased_declared_field_is_accepted() {
    let fixture = context_fixture();
    let plan = label_reading(
        &fixture,
        "ctxLocale: locale",
        reading(1, "Store", "ctxLocale"),
    );
    fixture.check(&plan, STORE_OFFERS_LABEL).unwrap();
}

// A type guard after the last key, on the value itself: the `locale` read is a `String`, no object
// matches the guard, and the router supplies no argument. The model's
// `typeGuardAfterTheLeafIsRejected`.
//
// A control of changed behaviour: a copy narrowed to nothing at the level its path ends at was
// still a copy, and the value was found.
#[test]
fn a_type_guard_after_the_leaf_is_rejected() {
    let fixture = context_fixture();
    let mut rewrite = reading(1, "Store", "locale");
    rewrite
        .as_array_mut()
        .expect("a path")
        .push(serde_json::json!({ "TypenameEquals": "Offer" }));
    let plan = label_reading(&fixture, "locale", rewrite);
    assert_rejected_for(&fixture, &plan, STORE_OFFERS_LABEL, NOT_FETCHED);
}

// No department's offers are fetched, so the fetch runs at the stores' offers alone, and its
// rewrite, written for either member, reads the store's `locale`. The department's `locale` key,
// which holds its currency, is one no execution of the fetch reads, and is not judged. The
// planner's plan for the stores' offers, with the departments' `locale: currency` fetched and the
// rewrite's type guard dropped. The model's `rewriteReadsOnlyWhereTheFetchRunsIsAccepted`.
//
// A control of preserved behaviour, which a reading judging every copy the path reaches would fail.
#[test]
fn a_rewrite_reads_only_where_the_fetch_runs_and_is_accepted() {
    let fixture = context_fixture();
    let plan = edited_plan(&fixture, plan_json!("outlets_store_offers_label"), |plan| {
        replace_once(
            plan,
            DIRECTORY_OPERATION,
            "offers { __typename id } } } }",
            "offers { __typename id } } ... on Department { locale: currency } } }",
        );
        set(
            plan,
            &rewrite_path(0),
            serde_json::json!(["Parent", { "Key": ["locale", null] }]),
        );
    });
    fixture
        .check(&plan, "{ outlets { ... on Store { offers { label } } } }")
        .unwrap();
}

const STORE_OFFERS_PRICE: &str = "{ regions { stores { offers { price } } } }";

// Each of `price`'s arguments is bound by its own rewrite to the leaf its declaration selects off
// the store. The planner's plan. The model's `twoArgumentsEachBoundToTheirOwnLeafAreAccepted`.
//
// A control of preserved behaviour.
#[test]
fn two_arguments_each_bound_to_their_own_leaf_are_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("regions_stores_offers_price"));
    fixture.check(&plan, STORE_OFFERS_PRICE).unwrap();
}

// Each variable is bound to the other's leaf, so `stores` is sent the currency as `locale` and the
// locale as `currency`. Both are `String` and both are fetched, so only the declaration tells. The
// model's `twoArgumentsBoundToEachOthersLeavesAreRejected`.
//
// A control of changed behaviour.
#[test]
fn two_arguments_bound_to_each_others_leaves_are_rejected() {
    let fixture = context_fixture();
    let plan = edited_plan(
        &fixture,
        plan_json!("regions_stores_offers_price"),
        |plan| {
            set(plan, &rewrite_path(0), reading(1, "Store", "currency"));
            set(plan, &rewrite_path(1), reading(1, "Store", "locale"));
        },
    );
    assert_rejected_for(&fixture, &plan, STORE_OFFERS_PRICE, NOT_A_LEAF);
}

// The rewrite climbs past the store to the region, which does not set `locality`, and reads the
// region's own `locale`, which is fetched. The model's `rewriteClimbingToANonSetterIsRejected`.
//
// A control of changed behaviour.
#[test]
fn a_rewrite_climbing_to_a_non_setter_is_rejected() {
    let fixture = context_fixture();
    let plan = edited_plan(
        &fixture,
        plan_json!("regions_stores_offers_label"),
        |plan| {
            replace_once(
                plan,
                DIRECTORY_OPERATION,
                "{ regions { stores {",
                "{ regions { __typename locale stores {",
            );
            set(plan, &rewrite_path(0), reading(2, "Region", "locale"));
        },
    );
    assert_rejected_for(
        &fixture,
        &plan,
        STORE_OFFERS_LABEL,
        "which climbs to `Region`, a type declaring no context `locality`",
    );
}

// The rewrite reads `locale` off the department, the nearest of the two objects above the offer
// that set `locality`. The planner's plan. The model's `rewriteReadingTheNearestSetterIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn a_rewrite_reading_the_nearest_setter_is_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("regions_stores_departments_offers_label"));
    fixture
        .check(
            &plan,
            "{ regions { stores { departments { offers { label } } } } }",
        )
        .unwrap();
}

/// A planner's plan for the offers' `label` below each store and department, an interface's members
/// or a union's, with the department's rewrite dropped.
fn department_unread(plan: &str, fixture: &Fixture) -> QueryPlan {
    edited_plan(fixture, plan, |plan| {
        plan.pointer_mut(&format!("{STORES_FETCH}/context_rewrites"))
            .and_then(serde_json::Value::as_array_mut)
            .expect("the rewrites")
            .remove(1);
    })
}

/// The same plan with the department's rewrite reading its `currency`, which `directory` then
/// fetches.
fn department_misread(plan: &str, fixture: &Fixture) -> QueryPlan {
    edited_plan(fixture, plan, |plan| {
        replace_once(
            plan,
            DIRECTORY_OPERATION,
            "... on Department { __typename locale offers",
            "... on Department { __typename locale currency offers",
        );
        set(plan, &rewrite_path(1), reading(1, "Department", "currency"));
    })
}

const OUTLETS_MEMBERS_OFFERS_LABEL: &str =
    "{ outlets { ... on Store { offers { label } } ... on Department { offers { label } } } }";

// An outlet is a store or a department, both setting `locality`: one variable is bound by a rewrite
// per type, each reading the declared leaf, and the router takes the value from the one that finds
// it. The planner's plan, which selects the interface's members by fragment; the model selects the
// interface's fields unsplit, which the planner does not plan. The model's
// `oneRewritePerSetterIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn one_rewrite_per_setter_is_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("outlets_members_offers_label"));
    fixture.check(&plan, OUTLETS_MEMBERS_OFFERS_LABEL).unwrap();
}

// Only the store's rewrite remains, so an offer under a department is sent no `locale`. The model's
// `oneSetterLeftUnreadIsRejected`.
//
// A control of preserved behaviour.
#[test]
fn one_setter_left_unread_is_rejected() {
    let fixture = context_fixture();
    let plan = department_unread(plan_json!("outlets_members_offers_label"), &fixture);
    assert_rejected_for(&fixture, &plan, OUTLETS_MEMBERS_OFFERS_LABEL, NOT_FETCHED);
}

// The department's rewrite reads `currency` where the declaration selects `{ locale }`. Every
// outlet still binds a value, so only the declaration tells. The model's
// `oneSetterMisreadIsRejected`.
//
// A control of changed behaviour.
#[test]
fn one_setter_misread_is_rejected() {
    let fixture = context_fixture();
    let plan = department_misread(plan_json!("outlets_members_offers_label"), &fixture);
    assert_rejected_for(
        &fixture,
        &plan,
        OUTLETS_MEMBERS_OFFERS_LABEL,
        &format!("on `Department` holds `currency`, {NOT_A_LEAF}"),
    );
}

// A context declared on an interface is set by its members, which the rewrites read off, and the
// declaration's `locale` is read on the interface. The planner's plan for `note`, which takes its
// `locale` from `outlet`, declared on `Outlet`. The model's
// `contextDeclaredOnAnInterfaceIsReadOnItsMembersIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn a_context_declared_on_an_interface_is_read_on_its_members_and_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("outlets_offers_note"));
    fixture
        .check(&plan, "{ outlets { offers { note } } }")
        .unwrap();
}

const VENUES_OFFERS_LABEL: &str =
    "{ venues { ... on Store { offers { label } } ... on Department { offers { label } } } }";

// Both members of the union set `locality`, and a rewrite per member reads the declared leaf off
// it. The planner's plan. The model's `oneRewritePerUnionMemberIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn one_rewrite_per_union_member_is_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("venues_offers_label"));
    fixture.check(&plan, VENUES_OFFERS_LABEL).unwrap();
}

// Only the store's rewrite remains, so an offer under a department of the union is sent no
// `locale`. The model's `unionMemberLeftUnreadIsRejected`.
//
// A control of preserved behaviour.
#[test]
fn a_union_member_left_unread_is_rejected() {
    let fixture = context_fixture();
    let plan = department_unread(plan_json!("venues_offers_label"), &fixture);
    assert_rejected_for(&fixture, &plan, VENUES_OFFERS_LABEL, NOT_FETCHED);
}

// The department's rewrite reads `currency`, and the value is there: only the declaration tells,
// and only a path follower that keeps the member's object below the union's fragment reaches it.
// The model's `unionMemberMisreadIsRejected`.
//
// A control of changed behaviour.
#[test]
fn a_union_member_misread_is_rejected() {
    let fixture = context_fixture();
    let plan = department_misread(plan_json!("venues_offers_label"), &fixture);
    assert_rejected_for(
        &fixture,
        &plan,
        VENUES_OFFERS_LABEL,
        &format!("on `Department` holds `currency`, {NOT_A_LEAF}"),
    );
}

// A context declared on a union selects its `__typename`, which every union has though it declares
// no field, and each member's fetched tag is read by the rewrite on it. The planner's plan for
// `tag`, which takes its `kind` from `venue`, declared on `Venue`. The model's
// `contextOnAUnionReadsItsTypenameIsAccepted`.
//
// A control of preserved behaviour, which a reading looking `__typename` up among the declared
// fields would fail.
#[test]
fn a_context_on_a_union_reads_its_typename_and_is_accepted() {
    let fixture = context_fixture();
    let plan = fixture.written_plan(plan_json!("venues_offers_tag"));
    fixture
        .check(
            &plan,
            "{ venues { ... on Store { offers { tag } } ... on Department { offers { tag } } } }",
        )
        .unwrap();
}

//==================================================================================================
// A contextual value read off the object the fetch runs on
//
// The router applies an entity fetch's `requires` selection to each object the flatten path
// selects and sends a representation only where it yields a non-empty object, so the fetch runs on
// the objects of the types its `requires` entries are written at and no other, and a contextual
// value it reads off the object it runs on is owed for those objects alone. Each test below is the
// port of a theorem of the model's `OwnPosition` tests, named in its comment, over the world
// `own_position.graphql` describes: an accepted plan is the planner's, and a rejected one the
// planner's edited as its comment says, each negative reaching the availability or the reading
// obligation with formation, bindings and the client's completeness otherwise satisfied.

fn own_position_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/own_position.graphql"))
}

/// Where the planner's plans for the own-position world keep the first fetch's operation, from
/// `catalog`.
const CATALOG_OPERATION: &str = "/Sequence/nodes/0/Fetch/operation_document";

const ITEM_PRODUCT_LABEL: &str = "{ item { ... on Product { details { label } } } }";

const ITEM_MEMBERS_LABEL: &str =
    "{ item { ... on Product { details { label } } ... on Other { details { label } } } }";

/// The planner's plan for the product's `label` at `item`, with `catalog`'s selection of the
/// product's `title` written as `title`.
fn product_title_as(fixture: &Fixture, title: &str) -> QueryPlan {
    edited_plan(fixture, plan_json!("item_product_details_label"), |plan| {
        replace_once(
            plan,
            CATALOG_OPERATION,
            "... on Product { __typename id title }",
            &format!("... on Product {{ __typename id {title}}}"),
        );
    })
}

// The fetch is sent the products alone, each of which `catalog` answered with `title` under
// `... on Product`, and reads it off the object it runs on. The planner's plan. The model's
// `valueOffTheObjectTheFetchRunsOnIsAccepted`; the axes case `outlet-own-object`.
//
// A control of changed behaviour: the fetch's position stopped at the value `item` holds, so it had
// no level for the item, and the value, narrowed to `Product` there, served no configuration.
#[test]
fn a_value_off_the_object_the_fetch_runs_on_is_accepted() {
    let fixture = own_position_fixture();
    let plan = fixture.written_plan(plan_json!("item_product_details_label"));
    fixture.check(&plan, ITEM_PRODUCT_LABEL).unwrap();
}

// The same over a list of items: the level the fetch runs on holds an `Other` too, which is sent no
// representation and owes no value. The planner's plan. The model's
// `valueOffTheObjectsTheFetchRunsOnInAListIsAccepted`; the axes case `outlets-own-object`. Whether
// the router answers such a list correctly is shown by no run: it collects each object's context
// values before it filters and deduplicates the representations.
//
// A control of changed behaviour: the level was there but not narrowed to the types the fetch is
// sent, so an `Other` asked for the product's value.
#[test]
fn a_value_off_the_objects_the_fetch_runs_on_in_a_list_is_accepted() {
    let fixture = own_position_fixture();
    let plan = fixture.written_plan(plan_json!("items_product_details_label"));
    fixture
        .check(&plan, "{ items { ... on Product { details { label } } } }")
        .unwrap();
}

// `catalog` fetched no `title` for the products, so the value is missing where the fetch runs. The
// planner's plan with the products' `title` dropped. The model's
// `valueUnfetchedForTheObjectTheFetchRunsOnIsRejected`; the axes case `own-object-unfetched`.
//
// A control of preserved behaviour.
#[test]
fn a_value_unfetched_for_the_object_the_fetch_runs_on_is_rejected() {
    let fixture = own_position_fixture();
    let plan = product_title_as(&fixture, "");
    assert_rejected_for(&fixture, &plan, ITEM_PRODUCT_LABEL, NOT_FETCHED);
}

// The `requires` written at both members, with `title` fetched under each: every object the fetch
// is sent has its value. The planner's plan. The model's
// `valueFetchedUnderEachMemberSentIsAccepted`.
//
// A control of changed behaviour: each copy was narrowed past the one level the position had.
#[test]
fn a_value_fetched_under_each_member_sent_is_accepted() {
    let fixture = own_position_fixture();
    let plan = fixture.written_plan(plan_json!("item_members_details_label"));
    fixture.check(&plan, ITEM_MEMBERS_LABEL).unwrap();
}

// The same over a list of items. The planner's plan. The model's
// `valueFetchedUnderEachMemberSentInAListIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn a_value_fetched_under_each_member_sent_in_a_list_is_accepted() {
    let fixture = own_position_fixture();
    let plan = fixture.written_plan(plan_json!("items_members_details_label"));
    fixture
        .check(
            &plan,
            "{ items { ... on Product { details { label } } ... on Other { details { label } } } }",
        )
        .unwrap();
}

// The `requires` written at both members, with `title` fetched under the product alone: an `Other`
// the fetch is sent has no value. The planner's plan with the other's `title` dropped. The model's
// `valueMissingUnderOneMemberSentIsRejected`.
//
// A control of preserved behaviour.
#[test]
fn a_value_missing_under_one_member_sent_is_rejected() {
    let fixture = own_position_fixture();
    let plan = edited_plan(&fixture, plan_json!("item_members_details_label"), |plan| {
        replace_once(
            plan,
            CATALOG_OPERATION,
            "... on Other { __typename id title }",
            "... on Other { __typename id }",
        );
    });
    assert_rejected_for(&fixture, &plan, ITEM_MEMBERS_LABEL, NOT_FETCHED);
}

// `catalog` fetched `otherTitle` under the key `title` for the products: the value is there where
// the fetch runs, and it is the wrong field, which the reading, judged on the same configurations,
// tells. The planner's plan with the products' `title` fetched as `title: otherTitle`. The model's
// `wrongFieldOffTheObjectTheFetchRunsOnIsRejected`.
//
// The verdict is preserved and the asserted clause changed: before, availability rejected the
// plan, the fetch's position having no level for the item; the reading, quantifying over the same
// positions, had no configuration to judge the copy in.
#[test]
fn a_wrong_field_off_the_object_the_fetch_runs_on_is_rejected() {
    let fixture = own_position_fixture();
    let plan = product_title_as(&fixture, "title: otherTitle ");
    assert_rejected_for(
        &fixture,
        &plan,
        ITEM_PRODUCT_LABEL,
        &format!("on `Product` holds `otherTitle`, {NOT_A_LEAF}"),
    );
}

const CONCRETE_SUMMARY: &str = "{ concrete { details { summary } } }";

// `note` fetched under `... on Detail`: narrowed at a level below the levels the two paths share,
// which the fetch's path says nothing about, it serves nothing, since the product's `info` may be a
// `Summary`. The planner's plan with the `note` fetched under the fragment. The model's
// `valueNarrowedBelowTheSharedLevelsIsRejected`.
//
// A control of preserved behaviour.
#[test]
fn a_value_narrowed_below_the_shared_levels_is_rejected() {
    let fixture = own_position_fixture();
    let plan = edited_plan(&fixture, plan_json!("concrete_details_summary"), |plan| {
        replace_once(
            plan,
            CATALOG_OPERATION,
            "info { __typename note }",
            "info { __typename ... on Detail { note } }",
        );
    });
    assert_rejected_for(&fixture, &plan, CONCRETE_SUMMARY, NOT_FETCHED);
}

// `note` fetched on the interface serves every `info`. The planner's plan. The model's
// `valueUnnarrowedBelowTheSharedLevelsIsAccepted`.
//
// A control of preserved behaviour.
#[test]
fn a_value_unnarrowed_below_the_shared_levels_is_accepted() {
    let fixture = own_position_fixture();
    let plan = fixture.written_plan(plan_json!("concrete_details_summary"));
    fixture.check(&plan, CONCRETE_SUMMARY).unwrap();
}

/// `catalog`'s fetch from the planner's plan for the product's `label` at `item`, which selects
/// each product's `title`, followed by a fetch to `details` at `item` under the path condition
/// `condition`, asking for the product's `summary`, whose `info { note }` no fetch selects.
fn summary_fetch_under(fixture: &Fixture, condition: serde_json::Value) -> QueryPlan {
    let operation = "query($representations: [_Any!]!, $note: String) { _entities(representations: \
                     $representations) { ... on Product { details { summary(note: $note) } } } }";
    let summary = serde_json::json!({ "Flatten": {
        "path": [{ "Key": ["item", condition] }],
        "node": { "Fetch": {
            "subgraph_name": "details",
            "variable_usages": ["note"],
            "requires": [{
                "kind": "InlineFragment",
                "typeCondition": "Product",
                "selections": [
                    { "kind": "Field", "name": "__typename" },
                    { "kind": "Field", "name": "id" }
                ]
            }],
            "operation_document": operation,
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": [{ "KeyRenamer": {
                "path": [{ "Key": ["info", null] }, { "Key": ["note", null] }],
                "rename_key_to": "note"
            } }]
        } }
    } });
    edited_plan(fixture, plan_json!("item_product_details_label"), |plan| {
        set(plan, "/Sequence/nodes/1", summary);
    })
}

/// What the client asks for here: each product's `title`, which `catalog` fetches, so that no
/// fetch to `details` is needed for the client's demand.
const ITEM_PRODUCT_TITLE: &str = "{ item { ... on Product { title } } }";

/// The availability clause of the `summary` fetch's `note`.
const NOTE_NOT_FETCHED: &str =
    "the contextual value for `$note` is not always fetched where the fetch runs";

// A fetch whose flatten path ends at `item|[]` runs on no object: its own position, read to the
// object with the path's last condition narrowing it, reaches nothing, so no contextual value is
// owed for it, and the plan is accepted with the report of an empty condition off, though the
// `info { note }` the fetch's rewrite reads was never fetched. The client's `title` is
// `catalog`'s. The control of § Differences from the model's statement on a flatten path written
// to reach no type.
//
// A control of changed behaviour: the position was read to the value `item` holds, without that
// key's condition, and the value was owed there.
#[test]
fn a_fetch_reaching_no_object_owes_no_contextual_value() {
    let fixture = own_position_fixture();
    let plan = summary_fetch_under(&fixture, serde_json::json!([]));
    fixture
        .check_without_dead_fetch_report(&plan, ITEM_PRODUCT_TITLE)
        .unwrap();
}

// The same fetch at `item`, where it runs on the product: the value it reads was never fetched.
//
// A control of preserved behaviour.
#[test]
fn the_same_fetch_on_a_live_path_owes_its_contextual_value() {
    let fixture = own_position_fixture();
    let plan = summary_fetch_under(&fixture, serde_json::Value::Null);
    let error = fixture
        .check_without_dead_fetch_report(&plan, ITEM_PRODUCT_TITLE)
        .expect_err("the value is missing where the fetch runs");
    assert!(error.to_string().contains(NOTE_NOT_FETCHED), "{error}");
}

//==================================================================================================
// Result types read from the subgraphs writing a copy
//
// What a copy can be is read from every contribution to it: the possible runtime types each one's
// subgraph declares for the field at the subgraph type it was selected on, each standing for a
// supergraph object, intersected over the contributions active together and over the subgraphs
// sharing the field. Each test ports a theorem of the model's `Apollo/Tests/ProducerTypes.lean`,
// `ProducerRepairs.lean`, `ProducerMerging.lean` or `ProducerSharing.lean`, named beside it, or
// states a control of the pooled reading the model enumerates, and says whether its verdict is
// changed or preserved against the checker that read every position's possible types at the
// supergraph, narrowed by the subgraphs declaring the field.

/// A fetch asking `subgraph` for `operation` at the root.
fn root_fetch(subgraph: &str, operation: &str) -> serde_json::Value {
    serde_json::json!({ "Fetch": {
        "subgraph_name": subgraph,
        "variable_usages": [],
        "operation_document": operation,
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": []
    } })
}

/// The selections of a `requires` entry or a field of one: each a field name, or a field name with
/// what is selected under it.
fn entry_fields(fields: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(
        fields
            .iter()
            .map(|name| serde_json::json!({ "kind": "Field", "name": name }))
            .collect(),
    )
}

/// A fetch sent each entity of `entity_type` as `entry`, asking `subgraph` for `selection` of it.
fn entity_fetch(
    subgraph: &str,
    entity_type: &str,
    entry: serde_json::Value,
    selection: &str,
) -> serde_json::Value {
    serde_json::json!({ "Fetch": {
        "subgraph_name": subgraph,
        "variable_usages": [],
        "requires": [{
            "kind": "InlineFragment",
            "typeCondition": entity_type,
            "selections": entry
        }],
        "operation_document": format!(
            "query($representations: [_Any!]!) {{ _entities(representations: $representations) \
             {{ ... on {entity_type} {{ {selection} }} }} }}"
        ),
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": []
    } })
}

fn flatten(path: serde_json::Value, node: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "Flatten": { "path": path, "node": node } })
}

fn sequence(nodes: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "Sequence": { "nodes": nodes } })
}

fn parallel(nodes: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "Parallel": { "nodes": nodes } })
}

fn plan_of(fixture: &Fixture, node: serde_json::Value) -> QueryPlan {
    fixture.written_plan(&node.to_string())
}

/// A condition node running `if_clause` when `$variable` is true and `else_clause` when not.
fn condition(
    variable: &str,
    if_clause: Option<serde_json::Value>,
    else_clause: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({ "Condition": {
        "condition_variable": variable,
        "if_clause": if_clause,
        "else_clause": else_clause
    } })
}

/// A `defer` node: `primary` first, then each deferred block, naming the fetch ids it depends on.
fn defer(
    primary: serde_json::Value,
    deferred: Vec<(Vec<u64>, serde_json::Value)>,
) -> serde_json::Value {
    let deferred: Vec<serde_json::Value> = deferred
        .into_iter()
        .map(|(depends, node)| {
            let depends: Vec<serde_json::Value> = depends
                .iter()
                .map(|id| serde_json::json!({ "id": id.to_string() }))
                .collect();
            serde_json::json!({
                "depends": depends,
                "label": null,
                "query_path": [],
                "sub_selection": null,
                "node": node
            })
        })
        .collect();
    serde_json::json!({ "Defer": {
        "primary": { "sub_selection": null, "node": primary },
        "deferred": deferred
    } })
}

/// A fetch node with one of its fields set: its `id`, its operation kind, or its output or context
/// rewrites.
fn with(mut fetch: serde_json::Value, field: &str, value: serde_json::Value) -> serde_json::Value {
    fetch["Fetch"][field] = value;
    fetch
}

/// A key renamer at `path`, renaming the key it ends at to `to`.
fn renamer(path: serde_json::Value, to: &str) -> serde_json::Value {
    serde_json::json!({ "KeyRenamer": { "path": path, "rename_key_to": to } })
}

//--------------------------------------------------------------------------------------------------
// A field with no possible object result types owes nothing below it, and is still owed

fn no_object_result_types_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/no_object_result_types.graphql"))
}

const THING_NAME: &str = "{ thing { name } }";

// `a` returns `thing` without its `name`: `thing` has no possible object result types in `a`, so
// nothing below it is owed.
//
// The model's `noPossibleObjectResultTypesIsAccepted`. A control of changed behaviour.
#[test]
fn a_field_with_no_possible_object_result_types_owes_nothing_below_it() {
    let fixture = no_object_result_types_fixture();
    let plan = plan_of(&fixture, root_fetch("a", "{ thing { __typename } }"));
    fixture.check(&plan, THING_NAME).unwrap();
}

// The same plan where `a` declares `Item` too: `thing` from `a` can be an `Item`, whose `name` was
// not fetched.
//
// The model's `addedPossibleObjectResultTypeIsRejected`. A control of preserved behaviour.
#[test]
fn a_field_with_a_possible_object_result_type_owes_its_fields() {
    let fixture = Fixture::new(include_str!("testdata/an_object_result_type.graphql"));
    let plan = plan_of(&fixture, root_fetch("a", "{ thing { __typename } }"));
    let error = fixture.rejection(&plan, THING_NAME);
    assert!(
        error.contains("does not select `name` on `thing`, a `Item`"),
        "{error}"
    );
}

// Nothing below `thing` is owed, and `thing` itself still is.
//
// The model's `omittedEmptyProducerFieldIsRejected`. A control of preserved behaviour.
#[test]
fn a_field_with_no_possible_object_result_types_is_still_owed() {
    let fixture = no_object_result_types_fixture();
    let plan = plan_of(&fixture, root_fetch("a", "{ __typename }"));
    let error = fixture.rejection(&plan, THING_NAME);
    assert!(
        error.contains("does not select `thing` on the root `Query`"),
        "{error}"
    );
}

// The planner's plan: `thing`'s `name` fetched from `a`.
//
// A control of preserved behaviour.
#[test]
fn the_planners_plan_for_the_fields_name_is_accepted() {
    let fixture = no_object_result_types_fixture();
    fixture
        .check(&fixture.written_plan(plan_json!("thing_name")), THING_NAME)
        .unwrap();
}

// The planner's plan for a client asking only an `Item`'s `id`: `thing` alone, which no copy `a`
// mounts can be an `Item`.
//
// A control of changed behaviour.
#[test]
fn the_planners_plan_for_a_member_the_producer_cannot_return_is_accepted() {
    let fixture = no_object_result_types_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("thing_item_id")),
            "{ thing { ... on Item { id } } }",
        )
        .unwrap();
}

//--------------------------------------------------------------------------------------------------
// A list field whose declaring subgraphs give it different members

const SECTIONS: &str =
    "query Sections { sections { id ... on Hero { title } ... on Promo { blurb } } }";

fn shared_field_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/shared_field_fewer_members.graphql"))
}

// The planner asks `catalog` alone for `sections`, whose possible runtime types there are `Hero`
// alone: no `blurb` is owed.
//
// The model's `narrowerListProducerIsAccepted`. A control of changed behaviour.
#[test]
fn a_narrower_list_producer_owes_its_own_members_alone() {
    let fixture = shared_field_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("shared_sections")),
            SECTIONS,
        )
        .unwrap();
}

/// `sections` asked of `asked` without a `Promo`'s `blurb`, with each `Hero`'s `title` from
/// `catalog`.
fn sections_from(fixture: &Fixture, asked: &str) -> QueryPlan {
    plan_of(
        fixture,
        sequence(vec![
            root_fetch(asked, "{ sections { __typename id } }"),
            flatten(
                serde_json::json!([{ "Key": ["sections", null] }, { "AnyIndex": null }]),
                entity_fetch(
                    "catalog",
                    "Hero",
                    entry_fields(&["__typename", "id"]),
                    "title",
                ),
            ),
        ]),
    )
}

// `catalog` resolves `sections` too, its elements a `Hero` alone, and the two subgraphs serving one
// field return one value: whichever is asked, no element is a `Promo`, whose `blurb` is owed
// nowhere.
//
// The sharing clause over the model's `widenedListProducerIsRejected` input, whose world has no
// provider sharing `sections`. A control of changed behaviour.
#[test]
fn a_wider_list_producer_is_narrowed_by_the_subgraph_sharing_the_field() {
    let fixture = shared_field_fixture();
    fixture
        .check(&sections_from(&fixture, "promos"), SECTIONS)
        .unwrap();
}

// `promos` overrides `catalog`'s `sections`, so it is the one subgraph resolving it, and a
// `Promo`'s `blurb` is owed and not fetched.
//
// The model's `widenedListProducerIsRejected`. A control of preserved behaviour.
#[test]
fn a_wider_list_producer_resolving_the_field_alone_owes_every_member() {
    let fixture = Fixture::new(include_str!("testdata/shared_field_overridden.graphql"));
    let error = fixture.rejection(&sections_from(&fixture, "promos"), SECTIONS);
    assert!(
        error.contains("does not select `blurb` on `sections`, a `Promo`"),
        "{error}"
    );
}

// The planner's plans over the interface whose producers each declare their own member.
#[test]
fn the_planners_plans_over_producers_of_fewer_members_are_accepted() {
    let fixture = Fixture::new(include_str!("testdata/producer_fewer_members.graphql"));
    fixture
        .check(
            &fixture.written_plan(plan_json!("catalog_sections")),
            "query CatalogSections { sections { id ... on Hero { title } ... on Promo { blurb } } }",
        )
        .unwrap();
    fixture
        .check(
            &fixture.written_plan(plan_json!("promos_sections")),
            "query PromosSections { promos { id ... on Hero { title } ... on Promo { blurb } } }",
        )
        .unwrap();
}

// `promos` overrides `catalog`'s `sections` progressively: both keep resolving it, the one or the
// other as the label decides, so neither constrains the other, and a `Promo` from `promos` owes
// its `blurb`, which its complete neighbour fetches.
//
// A control of changed behaviour where it rejects, and of preserved behaviour where it accepts.
#[test]
fn a_progressive_overrides_two_declarations_do_not_constrain_each_other() {
    let fixture = Fixture::new(include_str!(
        "testdata/shared_field_progressive_override.graphql"
    ));
    let error = fixture.rejection(&sections_from(&fixture, "promos"), SECTIONS);
    assert!(
        error.contains("does not select `blurb` on `sections`, a `Promo`"),
        "{error}"
    );
    let complete = plan_of(
        &fixture,
        sequence(vec![
            root_fetch(
                "promos",
                "{ sections { __typename id ... on Promo { blurb } } }",
            ),
            flatten(
                serde_json::json!([{ "Key": ["sections", null] }, { "AnyIndex": null }]),
                entity_fetch(
                    "catalog",
                    "Hero",
                    entry_fields(&["__typename", "id"]),
                    "title",
                ),
            ),
        ]),
    );
    fixture.check(&complete, SECTIONS).unwrap();
}

// A third subgraph, `mirror`, resolves `sections` beside `promos`' progressive override of
// `catalog` and declares both members: it shares the field with each of the two, so both
// declarations constrain it, and asked of `mirror` an element is a `Hero` alone, as `catalog`
// declares, and no `blurb` is owed.
//
// The model's `Subgraphs.sharedFieldCases`, intersecting every provider's declaration. A control
// of changed behaviour.
#[test]
fn a_third_provider_is_constrained_by_both_declarations_of_a_progressive_override() {
    let fixture = Fixture::new(include_str!(
        "testdata/shared_field_progressive_third.graphql"
    ));
    fixture
        .check(&sections_from(&fixture, "mirror"), SECTIONS)
        .unwrap();
}

//--------------------------------------------------------------------------------------------------
// A requirement selecting into a union its supplier declares with fewer members

fn nested_requirement_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/nested_requirement.graphql"))
}

const PRODUCT_ESTIMATE: &str = "{ product { estimate } }";

/// The `requires` entry for `estimate`: `price` under each of its members, or under `FixedPrice`
/// alone when `clipped`.
fn price_entry(clipped: bool) -> serde_json::Value {
    let mut members = vec![serde_json::json!({
        "kind": "InlineFragment",
        "typeCondition": "FixedPrice",
        "selections": entry_fields(&["value"])
    })];
    if !clipped {
        members.push(serde_json::json!({
            "kind": "InlineFragment",
            "typeCondition": "RangePrice",
            "selections": entry_fields(&["minValue"])
        }));
    }
    serde_json::json!([
        { "kind": "Field", "name": "__typename" },
        { "kind": "Field", "name": "id" },
        { "kind": "Field", "name": "price", "selections": members }
    ])
}

/// `product` and its `price` from `sup`, whose `Price` is a `FixedPrice` alone, then `estimate`
/// from `req`, sent `price_entry(clipped)`.
fn estimate_from_sup(fixture: &Fixture, clipped: bool) -> QueryPlan {
    plan_of(
        fixture,
        sequence(vec![
            root_fetch(
                "sup",
                "{ product { __typename id price { __typename ... on FixedPrice { value } } } }",
            ),
            flatten(
                serde_json::json!([{ "Key": ["product", null] }]),
                entity_fetch("req", "Product", price_entry(clipped), "estimate"),
            ),
        ]),
    )
}

/// `product` from `sup` and its `price` from `oth`, whose `Price` can be either member, with the
/// members' fields `price_selection`, then `estimate` from `req`, sent `price_entry(clipped)`.
fn estimate_from_oth(fixture: &Fixture, price_selection: &str, clipped: bool) -> QueryPlan {
    plan_of(
        fixture,
        sequence(vec![
            root_fetch("sup", "{ product { __typename id } }"),
            flatten(
                serde_json::json!([{ "Key": ["product", null] }]),
                entity_fetch(
                    "oth",
                    "Product",
                    entry_fields(&["__typename", "id"]),
                    &format!("price {{ __typename {price_selection} }}"),
                ),
            ),
            flatten(
                serde_json::json!([{ "Key": ["product", null] }]),
                entity_fetch("req", "Product", price_entry(clipped), "estimate"),
            ),
        ]),
    )
}

// The entry omits the `RangePrice` branch the declaration selects, and the `price` supplied is
// `sup`'s, whose one possible runtime type is `FixedPrice`, whose `value` was fetched.
//
// The model's `clippedUnreachableRequirementIsAccepted`. A control of changed behaviour.
#[test]
fn a_clipped_entry_matches_where_the_supplier_cannot_return_the_member() {
    let fixture = nested_requirement_fixture();
    fixture
        .check(&estimate_from_sup(&fixture, true), PRODUCT_ESTIMATE)
        .unwrap();
}

// The whole entry, whose `RangePrice` branch no `price` from `sup` reaches, so its `minValue` is
// owed nowhere.
//
// The model's `fullUnreachableRequirementIsAccepted`. A control of changed behaviour.
#[test]
fn a_full_entry_owes_nothing_at_a_member_the_supplier_cannot_return() {
    let fixture = nested_requirement_fixture();
    fixture
        .check(&estimate_from_sup(&fixture, false), PRODUCT_ESTIMATE)
        .unwrap();
}

/// The members' fields `oth` is asked for under `price`: each member's, or `FixedPrice`'s alone.
const BOTH_MEMBERS: &str = "... on FixedPrice { value } ... on RangePrice { minValue }";
const FIXED_PRICE: &str = "... on FixedPrice { value }";

// `price` from `oth`, which declares both members, and the entry clipped. `sup` resolves `price`
// too, a `FixedPrice` alone, and the two subgraphs serving one field return one value: no `price`
// is a `RangePrice`, where the clipped representation would not hold what the subgraph demands.
//
// The model's `sharedClippedRequirement`, which replaced `clippedWidenedRequirementIsRejected` on
// this input. A control of changed behaviour.
#[test]
fn a_clipped_entry_matches_where_a_provider_sharing_the_field_excludes_the_member() {
    let fixture = nested_requirement_fixture();
    fixture
        .check(
            &estimate_from_oth(&fixture, BOTH_MEMBERS, true),
            PRODUCT_ESTIMATE,
        )
        .unwrap();
}

// `price` from `oth` with the `FixedPrice` member's `value` alone, and the whole entry: no `price`
// is a `RangePrice`, so no `minValue` is sent. This is the planner's plan, which reads the two
// declarations of the shareable `price` as agreeing on `sup`'s one member, the agreement the
// sharing clause states.
//
// The model's `sharedFullRequirement`, which replaced `fullWidenedRequirementIsRejected` on this
// input. A control of changed behaviour.
#[test]
fn the_planners_plan_reading_the_shared_price_as_its_narrower_provider_is_accepted() {
    let fixture = nested_requirement_fixture();
    fixture
        .check(
            &estimate_from_oth(&fixture, FIXED_PRICE, false),
            PRODUCT_ESTIMATE,
        )
        .unwrap();
    fixture
        .check(
            &fixture.written_plan(plan_json!("product_estimate")),
            PRODUCT_ESTIMATE,
        )
        .unwrap();
}

// `sup` declares both members too, so a `price` can be a `RangePrice` whichever subgraph is asked.
// Clipped, the representation does not hold what the subgraph demands there; whole, the
// `minValue` it reads was never fetched.
//
// The model's `bothWideClippedRejected` and `bothWideFullRejected`. Controls of preserved
// behaviour.
#[test]
fn entries_owe_the_member_every_provider_can_return() {
    let fixture = Fixture::new(include_str!("testdata/nested_requirement_widened.graphql"));
    let error = fixture.rejection(
        &estimate_from_oth(&fixture, BOTH_MEMBERS, true),
        PRODUCT_ESTIMATE,
    );
    assert!(
        error.contains(
            "the representation does not hold `minValue` at `product.price`, a `RangePrice`"
        ),
        "{error}"
    );
    let error = fixture.rejection(
        &estimate_from_oth(&fixture, FIXED_PRICE, false),
        PRODUCT_ESTIMATE,
    );
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// `sup` declares `price` a `FixedPrice` alone only as `@external`, which resolves nothing and
// constrains nothing: `oth`'s `price` can be a `RangePrice`.
//
// The model's `externalNeighborRejected`. A control of preserved behaviour.
#[test]
fn an_external_declaration_constrains_nothing() {
    let fixture = Fixture::new(include_str!("testdata/nested_requirement_external.graphql"));
    let error = fixture.rejection(
        &estimate_from_oth(&fixture, FIXED_PRICE, false),
        PRODUCT_ESTIMATE,
    );
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// `sup` declares no `Product.price`, and declares a `price` that is a `FixedPrice` alone on another
// type: `oth` is the one subgraph resolving `Product.price`, which can be a `RangePrice`.
//
// The model's `nonsharedNeighborRejected`, with a field of the same name at another parent. A
// control of preserved behaviour.
#[test]
fn a_field_of_the_same_name_at_another_parent_constrains_nothing() {
    let fixture = Fixture::new(include_str!(
        "testdata/nested_requirement_unrelated.graphql"
    ));
    let error = fixture.rejection(
        &estimate_from_oth(&fixture, FIXED_PRICE, false),
        PRODUCT_ESTIMATE,
    );
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// `oth` resolves `price` only through its `product`'s `@provides`, and `sup`, which resolves it
// outright and is not asked, still constrains it to a `FixedPrice`: no `RangePrice`'s `minValue` is
// owed.
//
// The model's `providedFieldUsesSharing`. A control of changed behaviour.
#[test]
fn a_field_resolved_through_a_window_is_constrained_by_its_providers() {
    let fixture = Fixture::new(include_str!("testdata/nested_requirement_provided.graphql"));
    fixture
        .check(
            &plan_of(
                &fixture,
                root_fetch(
                    "oth",
                    "{ product { price { ... on FixedPrice { value } } } }",
                ),
            ),
            "{ product { price { ... on FixedPrice { value } ... on RangePrice { minValue } } } }",
        )
        .unwrap();
}

// A provider that is not asked supplies no field: `sup` is asked for the product's `id` alone,
// under `price` for an `id`, or for `price` without its `__typename`.
//
// The model's `sharingCannotSupplyMissingField`, `sharingCannotChangeFieldIdentity` and
// `sharingCannotSupplyTypename`. Controls of preserved behaviour.
#[test]
fn a_provider_not_asked_supplies_no_field() {
    let fixture = nested_requirement_fixture();
    let price_value = "{ product { price { ... on FixedPrice { value } } } }";
    let error = fixture.rejection(
        &plan_of(&fixture, root_fetch("sup", "{ product { id } }")),
        price_value,
    );
    assert!(error.contains("does not select `price`"), "{error}");
    let error = fixture.rejection(
        &plan_of(&fixture, root_fetch("sup", "{ product { price: id } }")),
        price_value,
    );
    assert!(
        error.contains("selects `price` on `product`, a `Product` as `id`"),
        "{error}"
    );
    let error = fixture.rejection(
        &plan_of(
            &fixture,
            root_fetch(
                "sup",
                "{ product { price { ... on FixedPrice { value } } } }",
            ),
        ),
        "{ product { price { __typename } } }",
    );
    assert!(error.contains("does not select `__typename`"), "{error}");
}

// `whole` declares `I` as an interface object standing for `A` and `B`, and is asked for `child`'s
// `X` alone; `narrow` resolves `A.child`, an `X` alone, and constrains it at `A` and nowhere else.
// A `B`'s child can still be a `Y`, whose `other` `objects` has to be asked for.
//
// The model's `sharingUsesSupergraphParent` and `sharingDoesNotRestrictOtherParent`. Controls of
// changed and preserved behaviour.
#[test]
fn a_provider_constrains_the_field_at_its_own_parent_alone() {
    let fixture = Fixture::new(include_str!("testdata/sharing_per_parent.graphql"));
    let operation = "{ parent { child { ... on X { value } ... on Y { other } } } }";
    let plan = |objects: &str| {
        plan_of(
            &fixture,
            parallel(vec![
                root_fetch("whole", "{ parent { child { ... on X { value } } } }"),
                root_fetch("objects", objects),
            ]),
        )
    };
    fixture
        .check(
            &plan("{ parent { ... on B { child { ... on Y { other } } } } }"),
            operation,
        )
        .unwrap();
    let error = fixture.rejection(&plan("{ parent { __typename } }"), operation);
    assert!(
        error.contains("does not select `other` on `parent.child`, a `Y`"),
        "{error}"
    );
    assert!(error.contains("`parent` is `B`"), "{error}");
}

// `whole` declares `thing` an `I`, an interface object standing for `A` and `B`, and `objects` an
// `A`: the two declarations meet at the supergraph object `A`, so no `B`'s `other` is owed, and the
// `value` both declare is.
//
// The model's `groundedIntersectionAccepted` and `groundedMissingValueRejected`. Controls of
// changed and preserved behaviour.
#[test]
fn declarations_naming_a_result_differently_meet_at_the_supergraph_object() {
    let fixture = Fixture::new(include_str!("testdata/grounded_intersection.graphql"));
    let operation = "{ thing { value ... on B { other } } }";
    fixture
        .check(
            &plan_of(&fixture, root_fetch("whole", "{ thing { value } }")),
            operation,
        )
        .unwrap();
    let error = fixture.rejection(
        &plan_of(&fixture, root_fetch("whole", "{ thing { __typename } }")),
        operation,
    );
    assert!(
        error.contains("does not select `value` on `thing`"),
        "{error}"
    );
}

//--------------------------------------------------------------------------------------------------
// Contributions keep the rows their producer returned

fn row_contributions_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/row_contributions.graphql"))
}

const ROWS_RESULT: &str = "{ rows { ... on A { result } ... on B { result } } }";
const ROWS_CONTEXTUAL: &str =
    "{ rows { ... on A { info { contextual } } ... on B { info { contextual } } } }";

/// The planner's plan with `b`'s contribution of each `B`'s `datum` dropped.
fn without_b_datum(fixture: &Fixture, plan: &str) -> QueryPlan {
    edited_plan(fixture, plan, |plan| {
        let b_branch = plan
            .pointer_mut("/Sequence/nodes/1/Parallel/nodes/0/Sequence/nodes")
            .and_then(serde_json::Value::as_array_mut)
            .expect("the branch serving `B`");
        assert_eq!(
            b_branch[0].pointer("/Flatten/node/Fetch/subgraph_name"),
            Some(&serde_json::json!("b"))
        );
        b_branch.remove(0);
    })
}

// `a` and `b` each contribute the `datum` of their kind of row, and every row is sent its `result`
// input.
//
// The model's `bothContributionsSupplyRequirementIsAccepted`. A control of preserved behaviour.
#[test]
fn contributions_of_each_kind_supply_every_rows_requirement() {
    let fixture = row_contributions_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("rows_result")),
            ROWS_RESULT,
        )
        .unwrap();
}

// `a`, declaring `I` with `A` alone, makes the only contribution of a `datum`: the `B` rows `rows`
// mounted are still there, and still owe theirs.
//
// The model's `narrowerContributionLeavesRequirementIsRejected`. A control of preserved behaviour.
#[test]
fn a_narrower_contribution_leaves_the_other_rows_requirement_owed() {
    let fixture = row_contributions_fixture();
    let plan = without_b_datum(&fixture, plan_json!("rows_result"));
    let error = fixture.rejection(&plan, ROWS_RESULT);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// The same for the contextual `datum` each row's `info` reads.
//
// The model's `bothContributionsSupplyContextIsAccepted`. A control of preserved behaviour.
#[test]
fn contributions_of_each_kind_supply_every_rows_context() {
    let fixture = row_contributions_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("rows_contextual")),
            ROWS_CONTEXTUAL,
        )
        .unwrap();
}

// The model's `narrowerContributionLeavesContextIsRejected`. A control of preserved behaviour.
#[test]
fn a_narrower_contribution_leaves_the_other_rows_context_owed() {
    let fixture = row_contributions_fixture();
    let plan = without_b_datum(&fixture, plan_json!("rows_contextual"));
    let error = fixture.rejection(&plan, ROWS_CONTEXTUAL);
    assert!(error.contains(NOT_FETCHED), "{error}");
}

// The planner's plans over rows one subgraph produces and two contribute to.
#[test]
fn the_planners_plans_over_contributed_rows_are_accepted() {
    let fixture = Fixture::new(include_str!("testdata/enrichment_keeps_rows.graphql"));
    for (plan, operation) in [
        (
            plan_json!("items_both_kinds"),
            "query BothKinds { items { id name ... on Book { price } ... on Disc { units } } }",
        ),
        (
            plan_json!("items_book_price"),
            "query BookEnrichment { items { id name ... on Book { price } } }",
        ),
        (
            plan_json!("items_disc_units"),
            "query DiscEnrichment { items { id name ... on Disc { units } } }",
        ),
    ] {
        fixture
            .check(&fixture.written_plan(plan), operation)
            .unwrap();
    }
}

//--------------------------------------------------------------------------------------------------
// Fetches writing one key at one parent, and contributions to the copy it holds

fn independent_copies_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/independent_copies.graphql"))
}

const CHILDREN_VALUE: &str = "{ children { value } }";

/// `first` and `second` each asked for `children`, `second` with `second_selection`.
fn two_producers(fixture: &Fixture, second_selection: &str) -> QueryPlan {
    plan_of(
        fixture,
        parallel(vec![
            root_fetch("first", "{ children { __typename id value } }"),
            root_fetch(
                "second",
                &format!("{{ children {{ __typename id {second_selection} }} }}"),
            ),
        ]),
    )
}

// Both fetches write `children` at the root, and both fetch the children's `value`.
//
// The model's `independentProducersSupplyTheirOwnFieldsIsAccepted`. A control of preserved
// behaviour.
#[test]
fn independent_producers_supply_their_own_fields() {
    let fixture = independent_copies_fixture();
    fixture
        .check(&two_producers(&fixture, "value"), CHILDREN_VALUE)
        .unwrap();
}

// `second` fetches no `value`, and `first` does: both write `children` at the root, which the
// router's merge makes one list whose elements correspond index by index, so `first`'s `value`
// is on every child.
//
// The model's `parallelRootChildrenMergeIsAccepted`, which replaced
// `independentProducerCannotBorrowAFieldIsRejected`. A control of preserved behaviour.
#[test]
fn parallel_root_fetches_of_one_key_merge_into_one_copy() {
    let fixture = independent_copies_fixture();
    fixture
        .check(&two_producers(&fixture, ""), CHILDREN_VALUE)
        .unwrap();
}

// The planner's plan: `left` and `right` each flattened onto the children `first` mounted, so
// their contributions are on the same copies.
//
// The model's `sameCopyEntityContributionsAreAccepted`. A control of preserved behaviour.
#[test]
fn contributions_to_the_same_copies_combine() {
    let fixture = independent_copies_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("children_left_right")),
            "{ children { left right } }",
        )
        .unwrap();
}

//--------------------------------------------------------------------------------------------------
// The query root re-entered below a field

fn root_reentry_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/root_reentry.graphql"))
}

/// `orders` asked under the operation `kind` for `field`, whose `query` re-enters the `Query`
/// type, then `promos` asked for `selection` there.
fn reentered_query(fixture: &Fixture, kind: &str, field: &str, selection: &str) -> QueryPlan {
    let keyword = if kind == "mutation" { "mutation " } else { "" };
    plan_of(
        fixture,
        sequence(vec![
            with(
                root_fetch(
                    "orders",
                    &format!("{keyword}{{ {field} {{ id query {{ __typename }} }} }}"),
                ),
                "operation_kind",
                serde_json::json!(kind),
            ),
            flatten(
                serde_json::json!([{ "Key": [field, null] }, { "Key": ["query", null] }]),
                root_fetch("promos", &format!("{{ {selection} }}")),
            ),
        ]),
    )
}

// A query fetch flattened at a payload's `query` runs on the `Query` the payload returns, under a
// mutation as under a query: its operation is read at `promos`' query root, which stands for the
// supergraph's `Query` on that copy, so the `promotions` it fetches are there, and without them
// they are owed.
//
// The model's `FetchNode.producedResponse`, which reads a fetch without `requires` at its
// subgraph's query root on the position its path reaches, whose supergraph type is that of the
// position's last step (`ResponsePosition.objectType`). Controls of preserved behaviour.
#[test]
fn a_query_fetch_reentering_the_query_root_supplies_it() {
    let fixture = root_reentry_fixture();
    for (kind, field) in [("query", "lastOrder"), ("mutation", "placeOrder")] {
        let keyword = if kind == "mutation" { "mutation " } else { "" };
        let operation = format!("{keyword}{{ {field} {{ id query {{ promotions }} }} }}");
        fixture
            .check(
                &reentered_query(&fixture, kind, field, "promotions"),
                &operation,
            )
            .unwrap();
        let error = fixture.rejection(
            &reentered_query(&fixture, kind, field, "__typename"),
            &operation,
        );
        assert!(
            error.contains(&format!("does not select `promotions` on `{field}.query`")),
            "{error}"
        );
    }
}

//--------------------------------------------------------------------------------------------------
// A fetch's fragments read in its own subgraph

fn subgraph_membership_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/subgraph_membership.graphql"))
}

const ITEM_I_VALUE: &str = "{ item { ... on I { value } } }";

// The planner's plan: `source` asked for each member's `value` under its own name.
//
// The model's `localUnionMembersAreCoveredIsAccepted`. A control of preserved behaviour.
#[test]
fn each_subgraph_member_named_supplies_its_field() {
    let fixture = subgraph_membership_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("item_i_value")),
            ITEM_I_VALUE,
        )
        .unwrap();
}

// `source` asked for `... on I { value }`, which in `source` selects nothing on a `B`.
//
// The model's `locallyImpossibleFragmentCannotSupplyAFieldIsRejected`. A control of changed
// behaviour.
#[test]
fn a_fragment_the_subgraph_does_not_apply_supplies_nothing() {
    let fixture = subgraph_membership_fixture();
    let plan = plan_of(
        &fixture,
        root_fetch("source", "{ item { __typename ... on I { value } } }"),
    );
    let error = fixture.rejection(&plan, ITEM_I_VALUE);
    assert!(
        error.contains("does not select `value` on `item`, a `B`"),
        "{error}"
    );
}

//--------------------------------------------------------------------------------------------------
// A path condition reads a fetched `__typename` value

fn parent_child_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/parent_child.graphql"))
}

/// The clause of a path condition whose `__typename` value was not fetched.
const TYPENAME_NOT_FETCHED: &str = "which the plan has not fetched there";

/// The parent's child from `first`, with the parent's `__typename` when `with_typename` and the
/// child's `value` too when `complete`, then each child's `value` from `children` at
/// `parent|[Parent].child`, or at `parents.@|[Parent].child` when `indexed`.
fn child_value_plan(
    fixture: &Fixture,
    indexed: bool,
    with_typename: bool,
    complete: bool,
) -> QueryPlan {
    let holder = if indexed { "parents" } else { "parent" };
    let typename = if with_typename { "__typename " } else { "" };
    let value = if complete { " value" } else { "" };
    let path = if indexed {
        serde_json::json!([
            { "Key": ["parents", null] },
            { "AnyIndex": ["Parent"] },
            { "Key": ["child", null] }
        ])
    } else {
        serde_json::json!([{ "Key": ["parent", ["Parent"]] }, { "Key": ["child", null] }])
    };
    plan_of(
        fixture,
        sequence(vec![
            root_fetch(
                "first",
                &format!("{{ {holder} {{ {typename}child {{ __typename id{value} }} }} }}"),
            ),
            flatten(
                path,
                entity_fetch(
                    "children",
                    "Child",
                    entry_fields(&["__typename", "id"]),
                    "value",
                ),
            ),
        ]),
    )
}

// `parent|[Parent]` reads the parent's `__typename` value, which was not fetched: the router
// selects no parent there, and the fetch runs nowhere.
//
// The model's `missingParentTypenameIsRejected`. A control of changed behaviour.
#[test]
fn a_key_condition_without_its_typename_is_rejected() {
    let fixture = parent_child_fixture();
    let plan = child_value_plan(&fixture, false, false, false);
    let error = fixture.rejection(&plan, "{ parent { child { value } } }");
    assert!(error.contains(TYPENAME_NOT_FETCHED), "{error}");
}

// The model's `observedParentTypenameIsAccepted`. A control of preserved behaviour.
#[test]
fn a_key_condition_with_its_typename_is_accepted() {
    let fixture = parent_child_fixture();
    let plan = child_value_plan(&fixture, false, true, false);
    fixture
        .check(&plan, "{ parent { child { value } } }")
        .unwrap();
}

// The model's `missingIndexParentTypenameIsRejected`. A control of changed behaviour.
#[test]
fn an_index_condition_without_its_typename_is_rejected() {
    let fixture = parent_child_fixture();
    let plan = child_value_plan(&fixture, true, false, false);
    let error = fixture.rejection(&plan, "{ parents { child { value } } }");
    assert!(error.contains(TYPENAME_NOT_FETCHED), "{error}");
}

// The model's `observedIndexParentTypenameIsAccepted`. A control of preserved behaviour.
#[test]
fn an_index_condition_with_its_typename_is_accepted() {
    let fixture = parent_child_fixture();
    let plan = child_value_plan(&fixture, true, true, false);
    fixture
        .check(&plan, "{ parents { child { value } } }")
        .unwrap();
}

// The root fetch already supplies the client's `value`, and the fetch under the condition still
// owes the `__typename` value it reads.
//
// The model's `completeRootDoesNotExcuseMissingParentTypename`. A control of changed behaviour.
#[test]
fn a_covered_client_does_not_excuse_a_missing_typename() {
    let fixture = parent_child_fixture();
    let plan = child_value_plan(&fixture, false, false, true);
    let error = fixture.rejection(&plan, "{ parent { child { value } } }");
    assert!(error.contains(TYPENAME_NOT_FETCHED), "{error}");
}

// The model's `completeRootWithObservedParentTypenameIsAccepted`. A control of preserved behaviour.
#[test]
fn a_covered_client_with_the_typename_is_accepted() {
    let fixture = parent_child_fixture();
    let plan = child_value_plan(&fixture, false, true, true);
    fixture
        .check(&plan, "{ parent { child { value } } }")
        .unwrap();
}

//--------------------------------------------------------------------------------------------------
// Dropping a contextual argument keeps the ordinary ones

const DETAIL_VALUE: &str = r#"{ parent { child { detail { value(ordinary: "keep") } } } }"#;

// The planner's plan, passing `value` its contextual argument and the client's `ordinary`.
//
// The model's `ordinaryArgumentIsRetained`. A control of preserved behaviour.
#[test]
fn an_ordinary_argument_beside_a_contextual_one_is_kept() {
    let fixture = parent_child_fixture();
    fixture
        .check(
            &fixture.written_plan(plan_json!("parent_child_detail")),
            DETAIL_VALUE,
        )
        .unwrap();
}

// The same with `ordinary` passed another value than the client's.
//
// The model's `ordinaryArgumentMismatchIsRejected`. A control of preserved behaviour.
#[test]
fn an_ordinary_argument_passed_another_value_is_rejected() {
    let fixture = parent_child_fixture();
    let plan = edited_plan(&fixture, plan_json!("parent_child_detail"), |plan| {
        replace_once(
            plan,
            "/Sequence/nodes/1/Flatten/node/Fetch/operation_document",
            r#"ordinary: "keep""#,
            r#"ordinary: "wrong""#,
        );
    });
    let error = fixture.rejection(&plan, DETAIL_VALUE);
    assert!(error.contains(r#"ordinary: "wrong""#), "{error}");
}

//--------------------------------------------------------------------------------------------------
// Identical selections write one copy

/// Two branches each asking `first` for the parent's child, the second given each child's `value`
/// from `children` only when `contribute_second`.
fn identical_branches(fixture: &Fixture, contribute_second: bool) -> QueryPlan {
    let branch = |contribute: bool| {
        let mut nodes = vec![root_fetch(
            "first",
            "{ parent { __typename child { __typename id } } }",
        )];
        if contribute {
            nodes.push(flatten(
                serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["child", null] }]),
                entity_fetch(
                    "children",
                    "Child",
                    entry_fields(&["__typename", "id"]),
                    "value",
                ),
            ));
        }
        sequence(nodes)
    };
    plan_of(
        fixture,
        parallel(vec![branch(true), branch(contribute_second)]),
    )
}

// Both branches write `parent.child` at the root, so they write one child, and the first
// branch's contribution is on it.
//
// The model's `identicalCopyMergedContributionIsAccepted`, which replaced
// `identicalOccurrenceCannotBorrowEnrichment`. A control of preserved behaviour.
#[test]
fn one_branchs_contribution_is_on_the_child_both_branches_write() {
    let fixture = parent_child_fixture();
    fixture
        .check(
            &identical_branches(&fixture, false),
            "{ parent { child { value } } }",
        )
        .unwrap();
}

// The model's `identicalCopiesEachWithContributionsAreAccepted`. A control of preserved behaviour.
#[test]
fn identical_copies_each_given_a_contribution_are_accepted() {
    let fixture = parent_child_fixture();
    fixture
        .check(
            &identical_branches(&fixture, true),
            "{ parent { child { value } } }",
        )
        .unwrap();
}

//--------------------------------------------------------------------------------------------------
// A contextual value split among the object's runtime types, and one field selected twice

/// The owner profile plan with the root fetch's `owner` selections `owner_selections`.
fn owner_plan_selecting(fixture: &Fixture, owner_selections: &str, owners: &[&str]) -> QueryPlan {
    edited_plan(fixture, plan_json!("stores_products_price"), |plan| {
        set(
            plan,
            "/Sequence/nodes/0/Fetch/operation_document",
            serde_json::json!(format!(
                "{{ stores {{ __typename owner {{ __typename {owner_selections} }} \
                 products {{ __typename id }} }} }}"
            )),
        );
        set(
            plan,
            "/Sequence/nodes/1/Flatten/path/2/Key/1",
            serde_json::json!(owners),
        );
    })
}

const SPLIT_PROFILES: &str = "... on Person { profile { __typename id } } \
                              ... on Company { profile { __typename id } } ";

// Each owner's `profile` selected under its runtime type and given its `locale`: whichever an
// owner's runtime type is, its products are sent a `locale`.
//
// The model's `splitOwnerContextIsAccepted`. A control of changed behaviour.
#[test]
fn a_contextual_value_split_among_owner_types_is_accepted() {
    let fixture = owner_profile_fixture();
    let plan = owner_plan_selecting(&fixture, SPLIT_PROFILES, &["Person", "Company"]);
    fixture.check(&plan, STORES_PRODUCTS_PRICE).unwrap();
}

// The model's `missingOwnerContextIsRejected`. A control of preserved behaviour.
#[test]
fn a_contextual_value_split_and_missing_for_one_owner_type_is_rejected() {
    let fixture = owner_profile_fixture();
    let plan = owner_plan_selecting(&fixture, SPLIT_PROFILES, &["Person"]);
    let error = fixture.rejection(&plan, STORES_PRODUCTS_PRICE);
    assert!(error.contains(NOT_FETCHED), "{error}");
}

/// The owner's `profile` selected bare and again under `... on Person`, and the `locale` fetched
/// for a `Person`'s alone, with no fetch for `price`.
fn mixed_profile_copies(fixture: &Fixture) -> QueryPlan {
    let mut plan = owner_plan_selecting(
        fixture,
        "profile { __typename id } ... on Person { profile { __typename id } }",
        &["Person"],
    );
    let Some(TopLevelPlanNode::Sequence(sequence)) = &mut plan.node else {
        panic!("the owner profile plan is a sequence");
    };
    sequence.nodes.truncate(2);
    plan
}

// A `Person`'s profile is one copy, selected twice, and given its `locale`.
//
// The model's `mixedBareAndGuardedCopiesKeepNarrowing`. A control of preserved behaviour.
#[test]
fn a_field_selected_bare_and_under_a_fragment_is_one_copy() {
    let fixture = owner_profile_fixture();
    fixture
        .check(
            &mixed_profile_copies(&fixture),
            "{ stores { owner { ... on Person { profile { locale } } } } }",
        )
        .unwrap();
}

// A `Company`'s profile is selected bare alone, and given no `locale`.
//
// The model's `mixedBareCopyDoesNotContributeToExcludedOwner`. A control of preserved behaviour.
#[test]
fn a_bare_copy_is_not_given_what_another_owner_type_was() {
    let fixture = owner_profile_fixture();
    let error = fixture.rejection(
        &mixed_profile_copies(&fixture),
        "{ stores { owner { ... on Company { profile { locale } } } } }",
    );
    assert!(
        error.contains("does not select `locale` on `stores.owner.profile`, a `Profile`"),
        "{error}"
    );
    assert!(error.contains("`stores.owner` is `Company`"), "{error}");
}

//--------------------------------------------------------------------------------------------------
// One copy per parent and key

const PARENT_CHILD_VALUE: &str = "{ parent { child { value } } }";

/// Each child's `value` from `children`, flattened at `path`.
fn child_values_at(path: serde_json::Value) -> serde_json::Value {
    flatten(
        path,
        entity_fetch(
            "children",
            "Child",
            entry_fields(&["__typename", "id"]),
            "value",
        ),
    )
}

/// `left` and `right`, two aliases of `parent`, or of `parents` when `indexed`, each child given
/// its `value` under `left`, and under `right` too when `complete`.
fn distinct_parents(fixture: &Fixture, indexed: bool, complete: bool) -> QueryPlan {
    let holder = if indexed { "parents" } else { "parent" };
    let path = |alias: &str| {
        let mut path = vec![serde_json::json!({ "Key": [alias, null] })];
        if indexed {
            path.push(serde_json::json!({ "AnyIndex": null }));
        }
        path.push(serde_json::json!({ "Key": ["child", null] }));
        serde_json::Value::Array(path)
    };
    let mut nodes = vec![
        root_fetch(
            "first",
            &format!(
                "{{ left: {holder} {{ __typename child {{ __typename id }} }} \
                 right: {holder} {{ __typename child {{ __typename id }} }} }}"
            ),
        ),
        child_values_at(path("left")),
    ];
    if complete {
        nodes.push(child_values_at(path("right")));
    }
    plan_of(fixture, sequence(nodes))
}

// Two aliases of one field are two parents, each holding its own child: the `value` given to the
// children under `left` is not on those under `right`, whatever the selections' text, and the same
// for two lists.
//
// The model's `distinctParentCannotBorrow`, `distinctParentsComplete`, `distinctListCannotBorrow`
// and `distinctListsComplete`. Controls of preserved behaviour.
#[test]
fn distinct_parents_hold_distinct_children() {
    let fixture = parent_child_fixture();
    for (indexed, holder) in [(false, "parent"), (true, "parents")] {
        let operation = format!(
            "{{ left: {holder} {{ child {{ value }} }} right: {holder} {{ child {{ value }} }} }}"
        );
        let error = fixture.rejection(&distinct_parents(&fixture, indexed, false), &operation);
        assert!(
            error.contains("does not select `value` on `right.child`"),
            "{error}"
        );
        fixture
            .check(&distinct_parents(&fixture, indexed, true), &operation)
            .unwrap();
    }
}

fn correlated_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/correlated_children.graphql"))
}

// In `source`, an `A`'s child is an `X` and a `B`'s a `Y`: the pairs `(A, Y)` and `(B, X)` are
// never configurations, so neither owes the `other` the client asks there, and each pair that is
// owes its own `value`.
//
// The model's `pairedCasesComplete` and `pairedCaseCannotBorrow`. Controls of changed and
// preserved behaviour.
#[test]
fn a_childs_runtime_type_stays_paired_with_its_parents() {
    let fixture = correlated_fixture();
    let operation = "{ parent { \
        ... on A { child { ... on X { value } ... on Y { other } } } \
        ... on B { child { ... on Y { value } ... on X { other } } } } }";
    let plan = |b_child: &str| {
        plan_of(
            &fixture,
            root_fetch(
                "source",
                &format!(
                    "{{ parent {{ ... on A {{ child {{ value }} }} ... on B {{ child {{ {b_child} }} }} }} }}"
                ),
            ),
        )
    };
    fixture.check(&plan("value"), operation).unwrap();
    let error = fixture.rejection(&plan("other"), operation);
    assert!(
        error.contains("does not select `value` on `parent.child`, a `Y`"),
        "{error}"
    );
}

// The root fetch and `extra` each fetch one coordinate of the vertices `consumer` requires: the two
// write the same vertices, so `consumer` is sent both.
//
// The model's `fromToInputsMerge` and `vInputsMerge`. Controls of preserved behaviour.
#[test]
fn complementary_nested_inputs_merge_on_one_copy() {
    let fixture = Fixture::new(include_str!("testdata/nested_inputs.graphql"));
    for (vertices, field) in [(&["from", "to"][..], "fromTo"), (&["v"][..], "onV")] {
        let selecting = |coordinate: &str| {
            vertices
                .iter()
                .map(|vertex| format!("{vertex} {{ {coordinate} }}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut entry = vec![
            serde_json::json!({ "kind": "Field", "name": "__typename" }),
            serde_json::json!({ "kind": "Field", "name": "id" }),
        ];
        for vertex in vertices {
            entry.push(serde_json::json!({
                "kind": "Field", "name": vertex, "selections": entry_fields(&["x", "y"])
            }));
        }
        let plan = plan_of(
            &fixture,
            sequence(vec![
                root_fetch(
                    "root",
                    &format!("{{ edge {{ __typename id {} }} }}", selecting("x")),
                ),
                flatten(
                    serde_json::json!([{ "Key": ["edge", null] }]),
                    entity_fetch(
                        "extra",
                        "Edge",
                        entry_fields(&["__typename", "id"]),
                        &selecting("y"),
                    ),
                ),
                flatten(
                    serde_json::json!([{ "Key": ["edge", null] }]),
                    entity_fetch("consumer", "Edge", serde_json::Value::Array(entry), field),
                ),
            ]),
        );
        fixture
            .check(&plan, &format!("{{ edge {{ {field} }} }}"))
            .unwrap();
    }
}

// The child's `value` is fetched under `$take` by a second root fetch writing the same child: the
// client asking for it under `$take` is served, and asking for it unconditionally is not.
//
// The model's `guardedMergeAccepted` and `inactiveContributionRejected`. Controls of preserved
// behaviour.
#[test]
fn a_guarded_contribution_serves_its_guard_alone() {
    let fixture = parent_child_fixture();
    let plan = plan_of(
        &fixture,
        parallel(vec![
            root_fetch("first", "{ parent { __typename child { __typename id } } }"),
            condition(
                "take",
                Some(root_fetch("first", "{ parent { child { value } } }")),
                None,
            ),
        ]),
    );
    fixture
        .check(
            &plan,
            "query($take: Boolean!) { parent { child { value @include(if: $take) } } }",
        )
        .unwrap();
    let error = fixture.rejection(&plan, &declaring_booleans(["take"], PARENT_CHILD_VALUE));
    assert!(error.contains("does not select `value`"), "{error}");
    assert!(error.contains("¬$take"), "{error}");
}

//--------------------------------------------------------------------------------------------------
// Output renames, before what a fetch writes is merged

/// The parent's child fetched under `temporary` and renamed to `to`, beside a fetch writing the
/// child's `seed` under `child`, then each `parent.child` given its `value`.
fn renamed_child(fixture: &Fixture, to: &str) -> QueryPlan {
    let first = with(
        root_fetch("first", "{ parent { temporary: child { __typename id } } }"),
        "output_rewrites",
        serde_json::json!([renamer(
            serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["temporary", null] }]),
            to
        )]),
    );
    plan_of(
        fixture,
        sequence(vec![
            parallel(vec![
                first,
                root_fetch("first", "{ parent { child { seed } } }"),
            ]),
            child_values_at(
                serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["child", null] }]),
            ),
        ]),
    )
}

// Renamed to `child`, the first fetch's child is the copy the second writes, and holds the key the
// entity fetch is sent; renamed to `other`, it is another copy, and `parent.child` holds no key.
//
// The model's `renamedChildMerges` and `renamedDifferentChildCannotBorrow`. Controls of
// preserved behaviour.
#[test]
fn a_renamed_key_merges_where_its_final_key_does() {
    let fixture = parent_child_fixture();
    fixture
        .check(&renamed_child(&fixture, "child"), PARENT_CHILD_VALUE)
        .unwrap();
    let error = fixture.rejection(&renamed_child(&fixture, "other"), PARENT_CHILD_VALUE);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// Two renames in order, the second written against the key the first left: `t1` becomes `t2`, then
// `child`.
//
// A control of preserved behaviour.
#[test]
fn renames_apply_in_order() {
    let fixture = parent_child_fixture();
    let fetch = with(
        root_fetch("first", "{ parent { t1: child { __typename id value } } }"),
        "output_rewrites",
        serde_json::json!([
            renamer(
                serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["t1", null] }]),
                "t2"
            ),
            renamer(
                serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["t2", null] }]),
                "child"
            ),
        ]),
    );
    fixture
        .check(&plan_of(&fixture, fetch), PARENT_CHILD_VALUE)
        .unwrap();
}

// A rename whose path names a runtime type moves that type's field alone: an `A`'s `c` becomes its
// `child`, and a `B`'s stays `c`. A client reading each type's child under its own key is served;
// one reading a `B`'s under `child` is not.
//
// Controls of preserved behaviour.
#[test]
fn a_rename_under_a_runtime_type_moves_that_types_field_alone() {
    let fixture = correlated_fixture();
    let fetch = with(
        root_fetch(
            "source",
            "{ parent { ... on A { c: child { value } } ... on B { c: child { value } } } }",
        ),
        "output_rewrites",
        serde_json::json!([renamer(
            serde_json::json!([
                { "Key": ["parent", null] },
                { "TypenameEquals": "A" },
                { "Key": ["c", null] }
            ]),
            "child"
        )]),
    );
    let plan = plan_of(&fixture, fetch);
    fixture
        .check(
            &plan,
            "{ parent { ... on A { child { ... on X { value } } } \
             ... on B { c: child { ... on Y { value } } } } }",
        )
        .unwrap();
    let error = fixture.rejection(
        &plan,
        "{ parent { ... on B { child { ... on Y { value } } } } }",
    );
    assert!(
        error.contains("does not select `child` on `parent`, a `B`"),
        "{error}"
    );
}

/// One selection of the parent's child under `c`, its `X`'s `value` and its `Y`'s `y_selection`,
/// renamed to `child` where the parent is an `A`.
fn shared_child_renamed_under_a(fixture: &Fixture, y_selection: &str) -> QueryPlan {
    plan_of(
        fixture,
        with(
            root_fetch(
                "source",
                &format!(
                    "{{ parent {{ c: child {{ ... on X {{ value }} ... on Y {{ {y_selection} }} }} }} }}"
                ),
            ),
            "output_rewrites",
            serde_json::json!([renamer(
                serde_json::json!([
                    { "Key": ["parent", null] },
                    { "TypenameEquals": "A" },
                    { "Key": ["c", null] }
                ]),
                "child"
            )]),
        ),
    )
}

// The parent's child is selected once, under `c`, and renamed to `child` where the parent is an
// `A`: an `A`'s child moves to `child` with what is selected below it, and a `B`'s stays under `c`
// with what is selected below it. A client reading each under its own key is served; with the
// `Y`'s `value` not selected, a `B`'s child owes it.
//
// A control of changed behaviour where it accepts, and of preserved behaviour where it rejects.
#[test]
fn a_shared_selection_renamed_under_a_runtime_type_moves_what_is_below_it() {
    let fixture = correlated_fixture();
    let operation = "{ parent { ... on A { child { ... on X { value } } } \
                     ... on B { c: child { ... on Y { value } } } } }";
    fixture
        .check(&shared_child_renamed_under_a(&fixture, "value"), operation)
        .unwrap();
    let error = fixture.rejection(
        &shared_child_renamed_under_a(&fixture, "__typename"),
        operation,
    );
    assert!(
        error.contains("does not select `value` on `parent.c`, a `Y`"),
        "{error}"
    );
}

//--------------------------------------------------------------------------------------------------
// Configurations correlated through a parent, across views

/// `consumer` sent each `parent.child` with its `value`, at either of its types, for its `score`.
fn scores_at_parent_child() -> serde_json::Value {
    let entry = |type_name: &str| {
        serde_json::json!({
            "kind": "InlineFragment",
            "typeCondition": type_name,
            "selections": entry_fields(&["__typename", "id", "value"])
        })
    };
    flatten(
        serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["child", null] }]),
        serde_json::json!({ "Fetch": {
            "subgraph_name": "consumer",
            "variable_usages": [],
            "requires": [entry("X"), entry("Y")],
            "operation_document": "query($representations: [_Any!]!) { _entities(representations: \
                $representations) { ... on X { score } ... on Y { score } } }",
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": []
        } }),
    )
}

const PARENT_CHILD_SCORES: &str = "{ parent { child { ... on X { score } ... on Y { score } } } }";

// The child is an `X` under an `A` and a `Y` under a `B`, and its `value` is fetched under the `A`
// alone: the `X`'s value does not serve the `Y`, so the `Y` is sent none. Fetched under both, every
// child is sent its own.
//
// Controls of preserved behaviour.
#[test]
fn a_value_fetched_under_one_parent_type_does_not_serve_the_other() {
    let fixture = correlated_fixture();
    let plan = |b_value: &str| {
        plan_of(
            &fixture,
            sequence(vec![
                root_fetch(
                    "source",
                    &format!(
                        "{{ parent {{ ... on A {{ child {{ __typename id value }} }} \
                         ... on B {{ child {{ __typename id {b_value} }} }} }} }}"
                    ),
                ),
                scores_at_parent_child(),
            ]),
        )
    };
    fixture.check(&plan("value"), PARENT_CHILD_SCORES).unwrap();
    let error = fixture.rejection(&plan(""), PARENT_CHILD_SCORES);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert!(error.contains("`parent` is `B`"), "{error}");
}

// The same, the scores deferred: the keys and each type's `value` are fetched by three fetches of
// the primary block, and the deferred block reads what the fetches it names send. Naming all three,
// every child's `value` is read; naming the keys' and the `A`'s, a `Y` is sent none, though the
// child's runtime types are what everything fetched before the block makes them.
//
// Controls of preserved behaviour.
#[test]
fn a_deferred_block_reads_each_parent_types_value_from_the_fetch_sending_it() {
    let fixture = correlated_fixture();
    let plan = |depends: Vec<u64>| {
        plan_of(
            &fixture,
            defer(
                sequence(vec![
                    with(
                        root_fetch(
                            "source",
                            "{ parent { ... on A { child { __typename id } } \
                             ... on B { child { __typename id } } } }",
                        ),
                        "id",
                        serde_json::json!(0),
                    ),
                    with(
                        root_fetch("source", "{ parent { ... on A { child { value } } } }"),
                        "id",
                        serde_json::json!(1),
                    ),
                    with(
                        root_fetch("source", "{ parent { ... on B { child { value } } } }"),
                        "id",
                        serde_json::json!(2),
                    ),
                ]),
                vec![(depends, scores_at_parent_child())],
            ),
        )
    };
    fixture
        .check(&plan(vec![0, 1, 2]), PARENT_CHILD_SCORES)
        .unwrap();
    let error = fixture.rejection(&plan(vec![0, 1]), PARENT_CHILD_SCORES);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert!(error.contains("`parent` is `B`"), "{error}");
}

/// The parent's child and its next from `source` (fetch 0), a `B`'s key again (fetch 1), each
/// `Z`'s `zval` from `zed` at `parent.child.next` (fetch 2), then a deferred block naming
/// `depends` and giving each `B` its `score` from `consumer`.
fn deferred_scores(fixture: &Fixture, depends: Vec<u64>) -> QueryPlan {
    let b_entry = serde_json::json!([{
        "kind": "InlineFragment",
        "typeCondition": "B",
        "selections": [
            { "kind": "Field", "name": "__typename" },
            { "kind": "Field", "name": "id" },
            { "kind": "Field", "name": "child", "selections": [{
                "kind": "InlineFragment",
                "typeCondition": "X",
                "selections": entry_fields(&["id"])
            }] }
        ]
    }]);
    let mut score = entity_fetch("consumer", "B", serde_json::Value::Null, "score");
    score["Fetch"]["requires"] = b_entry;
    plan_of(
        fixture,
        defer(
            sequence(vec![
                with(
                    root_fetch(
                        "source",
                        "{ parent { __typename \
                         ... on A { child { __typename id next { __typename id } } } \
                         ... on B { child { __typename id next { __typename id } } } } }",
                    ),
                    "id",
                    serde_json::json!(0),
                ),
                with(
                    root_fetch("source", "{ parent { ... on B { __typename id } } }"),
                    "id",
                    serde_json::json!(1),
                ),
                flatten(
                    serde_json::json!([
                        { "Key": ["parent", null] },
                        { "Key": ["child", null] },
                        { "Key": ["next", null] }
                    ]),
                    with(
                        entity_fetch("zed", "Z", entry_fields(&["__typename", "id"]), "zval"),
                        "id",
                        serde_json::json!(2),
                    ),
                ),
            ]),
            vec![(
                depends,
                flatten(serde_json::json!([{ "Key": ["parent", null] }]), score),
            )],
        ),
    )
}

// Fetch 2 adds a `zval` only where the parent is an `A`, its child an `X` and the child's next a
// `Z`: what it sends a deferred block is that addition with the fields leading to it, an `A`'s
// `child` among them and a `B`'s not, which leads to a `Y`, whose next is never a `Z`. A `B`'s
// `score` requires its `child`, so a block naming fetch 2 and the key's fetch 1 cannot read it;
// naming fetch 0, which wrote it, it can.
//
// A control of changed behaviour where it rejects, and of preserved behaviour where it accepts.
#[test]
fn a_deferred_dependency_sends_only_the_fields_leading_to_its_additions() {
    let fixture = Fixture::new(include_str!("testdata/deferred_ancestry.graphql"));
    let operation = "{ parent { ... on B { score } } }";
    fixture
        .check(&deferred_scores(&fixture, vec![0, 1, 2]), operation)
        .unwrap();
    let error = fixture.rejection(&deferred_scores(&fixture, vec![1, 2]), operation);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert!(
        error.contains("the representation reads `child` at `parent`, a `B`"),
        "{error}"
    );
}

/// Each `entity`'s `score` from `consumer`, sent its key and its `child`'s `id` where the child is
/// a `Y`.
fn score_where_the_child_is_a_y(entity: &str) -> serde_json::Value {
    let mut score = entity_fetch("consumer", entity, serde_json::Value::Null, "score");
    score["Fetch"]["requires"] = serde_json::json!([{
        "kind": "InlineFragment",
        "typeCondition": entity,
        "selections": [
            { "kind": "Field", "name": "__typename" },
            { "kind": "Field", "name": "id" },
            { "kind": "Field", "name": "child", "selections": [{
                "kind": "InlineFragment",
                "typeCondition": "Y",
                "selections": entry_fields(&["id"])
            }] }
        ]
    }]);
    score
}

/// The parent's keys (fetch 0); its `child` from `source` under `$b`, an `X` (fetch 1), and
/// otherwise its `y` aliased to `child`, a `Y` (fetch 2); then under `¬$b` each child's value
/// from `values`, for either member (fetch 3); and a deferred block naming `depends` and giving
/// the parent its `score` from `consumer` under `$b`.
fn aliased_children_scores(fixture: &Fixture, depends: Vec<u64>) -> QueryPlan {
    let parent_child = serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["child", null] }]);
    let mut values = entity_fetch("values", "X", serde_json::Value::Null, "xval");
    values["Fetch"]["requires"] = serde_json::Value::Array(
        ["X", "Y"]
            .iter()
            .map(|member| {
                serde_json::json!({
                    "kind": "InlineFragment",
                    "typeCondition": member,
                    "selections": entry_fields(&["__typename", "id"])
                })
            })
            .collect(),
    );
    values["Fetch"]["operation_document"] = serde_json::json!(
        "query($representations: [_Any!]!) { _entities(representations: $representations) \
         { ... on X { xval } ... on Y { yval } } }"
    );
    plan_of(
        fixture,
        defer(
            sequence(vec![
                with(
                    root_fetch("source", "{ parent { __typename id } }"),
                    "id",
                    serde_json::json!(0),
                ),
                condition(
                    "b",
                    Some(with(
                        root_fetch("source", "{ parent { child { __typename id } } }"),
                        "id",
                        serde_json::json!(1),
                    )),
                    Some(with(
                        root_fetch("source", "{ parent { child: y { __typename id } } }"),
                        "id",
                        serde_json::json!(2),
                    )),
                ),
                condition(
                    "b",
                    None,
                    Some(flatten(
                        parent_child,
                        with(values, "id", serde_json::json!(3)),
                    )),
                ),
            ]),
            vec![(
                depends,
                condition(
                    "b",
                    Some(flatten(
                        serde_json::json!([{ "Key": ["parent", null] }]),
                        score_where_the_child_is_a_y("Parent"),
                    )),
                    None,
                ),
            )],
        ),
    )
}

// Fetch 3 runs where `$b` is false, where the child is the `Y` fetch 2 wrote, so it adds at a `Y`
// alone, and what it sends a deferred block is that with the fields leading to it: fetch 2's `y`,
// and not fetch 1's `child`, which leads to an `X` only where `$b` is true. A parent's `score`
// under `$b` requires its `child`, so a block naming fetch 3 and the keys' fetch 0 cannot read it;
// naming fetch 1, which wrote it, it can.
//
// Controls of preserved behaviour.
#[test]
fn a_deferred_dependency_sends_no_field_its_condition_excludes() {
    let fixture = Fixture::new(include_str!("testdata/aliased_children.graphql"));
    let operation = "query($b: Boolean!) { parent { score @include(if: $b) } }";
    fixture
        .check(&aliased_children_scores(&fixture, vec![0, 1, 3]), operation)
        .unwrap();
    let error = fixture.rejection(&aliased_children_scores(&fixture, vec![0, 3]), operation);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert!(
        error.contains("the representation reads `child` at `parent`"),
        "{error}"
    );
}

/// The holder's `b` under `¬$b` (fetch 1) and otherwise its `a` aliased to `b`, an `A` (fetch 0);
/// the `child` of each `B` there from `source` (fetch 2); then under `$b` each child's `xval` from
/// `values` (fetch 3); and a deferred block naming `depends` and giving the `B` its `score` from
/// `consumer` under `¬$b`.
fn aliased_parents_scores(fixture: &Fixture, depends: Vec<u64>) -> QueryPlan {
    let holder_b = serde_json::json!([{ "Key": ["holder", null] }, { "Key": ["b", null] }]);
    plan_of(
        fixture,
        defer(
            sequence(vec![
                condition(
                    "b",
                    Some(with(
                        root_fetch("source", "{ holder { b: a { __typename id } } }"),
                        "id",
                        serde_json::json!(0),
                    )),
                    Some(with(
                        root_fetch("source", "{ holder { b { __typename id } } }"),
                        "id",
                        serde_json::json!(1),
                    )),
                ),
                flatten(
                    holder_b.clone(),
                    with(
                        entity_fetch(
                            "source",
                            "B",
                            entry_fields(&["__typename", "id"]),
                            "child { __typename id }",
                        ),
                        "id",
                        serde_json::json!(2),
                    ),
                ),
                condition(
                    "b",
                    Some(flatten(
                        serde_json::json!([
                            { "Key": ["holder", null] },
                            { "Key": ["b", null] },
                            { "Key": ["child", null] }
                        ]),
                        with(
                            entity_fetch(
                                "values",
                                "X",
                                entry_fields(&["__typename", "id"]),
                                "xval",
                            ),
                            "id",
                            serde_json::json!(3),
                        ),
                    )),
                    None,
                ),
            ]),
            vec![(
                depends,
                condition(
                    "b",
                    None,
                    Some(flatten(holder_b, score_where_the_child_is_a_y("B"))),
                ),
            )],
        ),
    )
}

// Fetch 3 runs where `$b` is true, where the holder's `b` is the `A` fetch 0 wrote, which has no
// `child`, so it adds at no position and sends a deferred block nothing leading to one: not fetch
// 2's `child`, whose step agrees with fetch 3's additions, but whose parent is a `B` only where
// `$b` is false. A `B`'s `score` under `¬$b` requires its `child`, so a block naming fetch 3 and
// the keys' fetch 1 cannot read it; naming fetch 2, which wrote it, it can.
//
// Controls of preserved behaviour.
#[test]
fn a_deferred_dependency_sends_no_field_below_a_parent_its_condition_excludes() {
    let fixture = Fixture::new(include_str!("testdata/aliased_parents.graphql"));
    let operation = "query($b: Boolean!) { holder { b @skip(if: $b) { score } } }";
    fixture
        .check(&aliased_parents_scores(&fixture, vec![1, 2, 3]), operation)
        .unwrap();
    let error = fixture.rejection(&aliased_parents_scores(&fixture, vec![1, 3]), operation);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert!(
        error.contains("the representation reads `child` at `holder.b`"),
        "{error}"
    );
}

//--------------------------------------------------------------------------------------------------
// A contextual value read off a sibling branch

fn divergent_context_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/divergent_context.graphql"))
}

/// `consumer` asked for each `A`'s `info` at `setter.left|[A]`, its context read off the setter's
/// `right`.
fn info_from_right() -> serde_json::Value {
    flatten(
        serde_json::json!([{ "Key": ["setter", null] }, { "Key": ["left", ["A"]] }]),
        serde_json::json!({ "Fetch": {
            "subgraph_name": "consumer",
            "variable_usages": ["contextualArgument_1_0"],
            "requires": [{
                "kind": "InlineFragment",
                "typeCondition": "A",
                "selections": entry_fields(&["__typename", "id"])
            }],
            "operation_document": "query($representations: [_Any!]!, $contextualArgument_1_0: \
                String) { _entities(representations: $representations) { ... on A { \
                info(c: $contextualArgument_1_0) } } }",
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": [renamer(
                serde_json::json!([
                    "Parent",
                    { "TypenameEquals": "Setter" },
                    { "Key": ["right", null] },
                    { "Key": ["val", null] }
                ]),
                "contextualArgument_1_0"
            )]
        } }),
    )
}

const SETTER_LEFT_INFO: &str = "{ setter { left { ... on A { info } } } }";

/// The setter's `left` and `right`, with the `right`'s `val` under `right_selections`.
fn setter_fetch(right_selections: &str) -> serde_json::Value {
    root_fetch(
        "base",
        &format!(
            "{{ setter {{ left {{ __typename ... on A {{ id }} }} \
             right {{ __typename {right_selections} }} }} }}"
        ),
    )
}

// The fetch runs on a `left` that is an `A`, and reads the `val` of the same setter's `right`,
// which is an `A` or a `B` independently of the `left`. Fetched under the `right`'s `A` alone, it
// is missing where the `right` is a `B`; fetched for both, it is always there.
//
// Controls of preserved behaviour.
#[test]
fn a_context_on_a_sibling_branch_is_owed_whatever_that_branch_is() {
    let fixture = divergent_context_fixture();
    fixture
        .check(
            &plan_of(
                &fixture,
                sequence(vec![setter_fetch("val"), info_from_right()]),
            ),
            SETTER_LEFT_INFO,
        )
        .unwrap();
    let error = fixture.rejection(
        &plan_of(
            &fixture,
            sequence(vec![setter_fetch("... on A { val }"), info_from_right()]),
        ),
        SETTER_LEFT_INFO,
    );
    assert!(error.contains("is not always fetched"), "{error}");
    assert!(error.contains("`setter.right` is `B`"), "{error}");
}

// The same, deferred: the `right`'s `val` is fetched for each type by its own fetch of the primary
// block, and the deferred block reads what the fetches it names send.
//
// A control of changed behaviour where it accepts, and of preserved behaviour where it rejects.
#[test]
fn a_deferred_context_on_a_sibling_branch_is_read_from_the_fetches_named() {
    let fixture = divergent_context_fixture();
    let plan = |depends: Vec<u64>| {
        plan_of(
            &fixture,
            defer(
                sequence(vec![
                    with(setter_fetch(""), "id", serde_json::json!(0)),
                    with(
                        root_fetch("base", "{ setter { right { ... on A { val } } } }"),
                        "id",
                        serde_json::json!(1),
                    ),
                    with(
                        root_fetch("base", "{ setter { right { ... on B { val } } } }"),
                        "id",
                        serde_json::json!(2),
                    ),
                ]),
                vec![(depends, info_from_right())],
            ),
        )
    };
    fixture
        .check(&plan(vec![0, 1, 2]), SETTER_LEFT_INFO)
        .unwrap();
    let error = fixture.rejection(&plan(vec![0, 1]), SETTER_LEFT_INFO);
    assert!(error.contains("is not always fetched"), "{error}");
    assert!(error.contains("`setter.right` is `B`"), "{error}");
}

//--------------------------------------------------------------------------------------------------
// Guards and deferred views together

/// The parent's child from `first`, then, under `$v`, fetch 1 writing the child's key and `seed`
/// again, then a deferred block naming fetch 1 and giving each child its `result` from `consumer`,
/// at `path`, under `$v` when `guarded`.
fn deferred_results(fixture: &Fixture, path: serde_json::Value, guarded: bool) -> QueryPlan {
    let results = flatten(
        path,
        entity_fetch(
            "consumer",
            "Child",
            entry_fields(&["__typename", "id", "seed"]),
            "result",
        ),
    );
    let results = if guarded {
        condition("v", Some(results), None)
    } else {
        results
    };
    plan_of(
        fixture,
        defer(
            sequence(vec![
                with(
                    root_fetch("first", "{ parent { __typename child { __typename id } } }"),
                    "id",
                    serde_json::json!(0),
                ),
                condition(
                    "v",
                    Some(with(
                        root_fetch("first", "{ parent { child { __typename id seed } } }"),
                        "id",
                        serde_json::json!(1),
                    )),
                    None,
                ),
            ]),
            vec![(vec![1], results)],
        ),
    )
}

// Fetch 1 sends the child's `seed` under `$v` alone: a consumer run under `$v` is sent it, one run
// unconditionally is not, and one whose path reads the parent's `__typename`, which only fetch 0,
// not named, wrote, cannot read it.
//
// Controls of preserved behaviour, but for the parent's `__typename`, which is changed.
#[test]
fn a_deferred_block_reads_a_guarded_dependency_under_its_guard() {
    let fixture = Fixture::new(include_str!("testdata/deferred_seed.graphql"));
    let operation = "query($v: Boolean!) { parent { child { result @include(if: $v) } } }";
    let parent_child = serde_json::json!([{ "Key": ["parent", null] }, { "Key": ["child", null] }]);
    fixture
        .check(
            &deferred_results(&fixture, parent_child.clone(), true),
            operation,
        )
        .unwrap();
    let error = fixture.rejection(&deferred_results(&fixture, parent_child, false), operation);
    assert!(
        error.contains("the representation reads") && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert!(error.contains("¬$v"), "{error}");
    let error = fixture.rejection(
        &deferred_results(
            &fixture,
            serde_json::json!([{ "Key": ["parent", ["Parent"]] }, { "Key": ["child", null] }]),
            true,
        ),
        operation,
    );
    assert!(error.contains(TYPENAME_NOT_FETCHED), "{error}");
}

// `narrow` writes `holder.thing` under `$v`, declaring it an `X` alone, and `wide`, which declares
// it an `X` or a `Y`, is not narrowed by `narrow`, whose declaration is external: under `$v` the
// thing is an `X`, otherwise it can be a `Y`, whose `y` is owed there. Fetched where `$v` is false,
// it is served; fetched where it is true, it is not.
//
// A control of changed behaviour where it accepts, and of preserved behaviour where it rejects.
#[test]
fn a_guarded_contribution_narrows_the_copy_under_its_guard_alone() {
    let fixture = Fixture::new(include_str!("testdata/conditional_intersection.graphql"));
    let operation = &declaring_booleans(["v"], "{ holder { thing { ... on Y { y } } } }");
    let narrow = || root_fetch("narrow", "{ holder { thing { ... on X { x } } } }");
    let wide_y = || root_fetch("wide", "{ holder { thing { ... on Y { y } } } }");
    let plan = |under_v: Vec<serde_json::Value>, otherwise: Option<serde_json::Value>| {
        plan_of(
            &fixture,
            sequence(vec![
                root_fetch("wide", "{ holder { thing { __typename } } }"),
                condition("v", Some(sequence(under_v)), otherwise),
            ]),
        )
    };
    fixture
        .check(&plan(vec![narrow()], Some(wide_y())), operation)
        .unwrap();
    let error = fixture.rejection(&plan(vec![narrow(), wide_y()], None), operation);
    assert!(
        error.contains("does not select `y` on `holder.thing`, a `Y`"),
        "{error}"
    );
    assert!(error.contains("¬$v"), "{error}");
}

//--------------------------------------------------------------------------------------------------
// Families growing in depth
//
// Each family's operations and plans grow with a depth the schema does not: a copy's possible
// runtime types depend on its parent's, so the configurations multiply with depth while the plan's
// syntax does not. Each verdict is checked at small depths here, and the work each takes is read
// against its syntax by `growth`.

/// `{ root { next { … { inner } } } }`, `depth` levels of `next`.
fn nexts(depth: usize, inner: &str) -> String {
    format!(
        "{{ root {{ {}{inner}{} }} }}",
        "next { ".repeat(depth),
        " }".repeat(depth)
    )
}

fn covariant_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/covariant_chain.graphql"))
}

/// The chain fetched to `depth`, the last level's `leaf` under `leaf`.
fn covariant_chain(fixture: &Fixture, depth: usize, leaf: &str) -> QueryPlan {
    plan_of(fixture, root_fetch("chain", &nexts(depth, leaf)))
}

// An `A`'s `next` is an `I` and a `B`'s an `A`, so a level's possible runtime types depend on the
// one above: every level reached can be a `B`, below an `A`. The `leaf` fetched at the last level
// is served; fetched there under `A` alone, it is missing where that level is a `B`.
//
// Controls of preserved behaviour, whose work `work_grows_with_the_plans_syntax` reads.
#[test]
fn a_covariant_chain_owes_its_leaf_at_every_reachable_type() {
    let fixture = covariant_fixture();
    for depth in [1, 2, 5] {
        let operation = nexts(depth, "leaf");
        fixture
            .check(&covariant_chain(&fixture, depth, "leaf"), &operation)
            .unwrap();
        let error = fixture.rejection(
            &covariant_chain(&fixture, depth, "... on A { leaf }"),
            &operation,
        );
        assert!(error.contains("does not select `leaf`"), "{error}");
        assert!(error.contains(", a `B`"), "{error}");
    }
}

fn complementary_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/complementary_ancestors.graphql"))
}

/// What is selected at level `level` of `depth` for the leaf's `f{level}`, fetched under each of
/// `types` at that level, then the levels below.
fn complementary_level(
    level: usize,
    depth: usize,
    types: &dyn Fn(usize) -> Vec<&'static str>,
) -> String {
    let below_here = depth - 1 - level;
    let leaf = format!(
        "{}leaf {{ f{level}: f }}{}",
        "next { ".repeat(below_here),
        " }".repeat(below_here)
    );
    let mut selections: Vec<String> = types(level)
        .into_iter()
        .map(|type_name| format!("... on {type_name} {{ {leaf} }}"))
        .collect();
    if level + 1 < depth {
        selections.push(format!(
            "next {{ {} }}",
            complementary_level(level + 1, depth, types)
        ));
    } else {
        selections.push("leaf { __typename }".to_string());
    }
    selections.join(" ")
}

/// Every one of `depth` levels can be an `A` or a `B`, and the leaf's `f{level}` is fetched under
/// both of that level's types, except `f{missing}` under its `B`.
fn complementary_chain(fixture: &Fixture, depth: usize, missing: Option<usize>) -> QueryPlan {
    let types = move |level: usize| {
        if Some(level) == missing {
            vec!["A"]
        } else {
            vec!["A", "B"]
        }
    };
    plan_of(
        fixture,
        root_fetch(
            "chain",
            &format!("{{ root {{ {} }} }}", complementary_level(0, depth, &types)),
        ),
    )
}

/// The leaf below `depth` levels, every `f{level}` asked.
fn complementary_operation(depth: usize) -> String {
    let fields: Vec<String> = (0..depth).map(|level| format!("f{level}: f")).collect();
    nexts(depth - 1, &format!("leaf {{ {} }}", fields.join(" ")))
}

// Each `f{level}` is fetched by two contributions, one under each type its level can have: between
// them they are there whatever every level is. Remove one, and `f{level}` is missing where that
// level is a `B`.
//
// Controls of preserved behaviour, whose work `work_grows_with_the_plans_syntax` reads.
#[test]
fn complementary_contributions_cover_every_ancestor_type() {
    let fixture = complementary_fixture();
    for depth in [1, 2, 5] {
        let operation = complementary_operation(depth);
        fixture
            .check(&complementary_chain(&fixture, depth, None), &operation)
            .unwrap();
        let missing = depth / 2;
        let error = fixture.rejection(
            &complementary_chain(&fixture, depth, Some(missing)),
            &operation,
        );
        assert!(
            error.contains(&format!("does not select `f{missing}`")),
            "{error}"
        );
        assert!(error.contains("is `B`"), "{error}");
    }
}

fn independent_pairs_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/independent_pairs.graphql"))
}

/// What is selected at level `level` of `2 * pairs` and below, where `under(level)` is the type a
/// pair's leaf field is selected under at that level, if any, and the last level's `leaf`.
fn pair_level(level: usize, pairs: usize, under: &dyn Fn(usize) -> Option<&'static str>) -> String {
    if level == 2 * pairs {
        return "leaf { __typename }".to_string();
    }
    let mut selections = Vec::new();
    if let Some(type_name) = under(level) {
        let below_here = 2 * pairs - level;
        selections.push(format!(
            "... on {type_name} {{ {}leaf {{ f{}: f }}{} }}",
            "next { ".repeat(below_here),
            level / 2,
            " }".repeat(below_here)
        ));
    }
    selections.push(format!(
        "next {{ {} }}",
        pair_level(level + 1, pairs, under)
    ));
    selections.join(" ")
}

/// Pair `i`'s leaf field fetched under the pair's `C`, or under its `D` for `changed`.
fn independent_pairs_chain(fixture: &Fixture, pairs: usize, changed: Option<usize>) -> QueryPlan {
    let under = move |level: usize| {
        (!level.is_multiple_of(2)).then(|| if Some(level / 2) == changed { "D" } else { "C" })
    };
    plan_of(
        fixture,
        root_fetch(
            "chain",
            &format!("{{ root {{ {} }} }}", pair_level(0, pairs, &under)),
        ),
    )
}

/// Pair `i`'s leaf field asked under the pair's `A`.
fn independent_pairs_operation(pairs: usize) -> String {
    let under = |level: usize| level.is_multiple_of(2).then_some("A");
    format!("{{ root {{ {} }} }}", pair_level(0, pairs, &under))
}

// Each pair of levels is an `A` over a `C` or a `B` over a `D`, independently of the others. Each
// pair's field is asked under its `A` and fetched under its `C`: wherever it is asked, it is there.
// Fetched under one pair's `D` instead, it is missing where that pair is an `A` over a `C`.
//
// Controls of preserved behaviour, whose work `work_grows_with_the_plans_syntax` reads.
#[test]
fn independent_pairs_each_supply_their_own_field() {
    let fixture = independent_pairs_fixture();
    for pairs in [1, 2, 4] {
        let operation = independent_pairs_operation(pairs);
        fixture
            .check(&independent_pairs_chain(&fixture, pairs, None), &operation)
            .unwrap();
        let changed = pairs / 2;
        let error = fixture.rejection(
            &independent_pairs_chain(&fixture, pairs, Some(changed)),
            &operation,
        );
        assert!(
            error.contains(&format!("does not select `f{changed}`")),
            "{error}"
        );
    }
}

fn member_fragments_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/member_fragments.graphql"))
}

/// `leaf { v }` under each of the first `members` members of `Outer`, and only `leaf`'s
/// `__typename` under `missing`'s.
fn member_fragments(members: usize, missing: Option<usize>) -> String {
    let selections: Vec<String> = (1..=members)
        .map(|member| {
            let leaf = if Some(member) == missing {
                "leaf { __typename }"
            } else {
                "leaf { v }"
            };
            format!("... on O{member} {{ {leaf} }}")
        })
        .collect();
    format!("{{ root {{ {} }} }}", selections.join(" "))
}

// The fetch writes one `leaf` copy under every member of `Outer` it selects, and `v` there under
// each, at every implementation of `Leaf` the copy can be: wherever the client asks for it, it is
// there. Left out under one member, it is missing where `root` is that member.
//
// Controls of preserved behaviour, whose work `work_grows_with_the_plans_syntax` reads.
#[test]
fn a_field_selected_under_each_members_fragment_is_supplied_under_each() {
    let fixture = member_fragments_fixture();
    for members in [1, 2, 8] {
        let operation = member_fragments(members, None);
        fixture
            .check(
                &plan_of(&fixture, root_fetch("chain", &operation)),
                &operation,
            )
            .unwrap();
        let missing = members / 2 + 1;
        let error = fixture.rejection(
            &plan_of(
                &fixture,
                root_fetch("chain", &member_fragments(members, Some(missing))),
            ),
            &operation,
        );
        assert!(error.contains("does not select `v`"), "{error}");
        assert!(error.contains(&format!("`O{missing}`")), "{error}");
    }
}

/// Below each of the first eight members of `Outer`, `leaf` and then `depth` levels of
/// `... on L1 { next { … } }` down to `v`, and only that copy's `__typename` under `missing`'s.
fn member_chains(depth: usize, missing: Option<usize>) -> String {
    let selections: Vec<String> = (1..=8)
        .map(|member| {
            let inner = if Some(member) == missing {
                "__typename"
            } else {
                "v"
            };
            format!(
                "... on O{member} {{ leaf {{ {}{inner}{} }} }}",
                "... on L1 { next { ".repeat(depth),
                " } }".repeat(depth)
            )
        })
        .collect();
    format!("{{ root {{ {} }} }}", selections.join(" "))
}

/// Under each of the first `members` members of `Outer`, `v` under the `leaf`'s implementation of
/// the same number.
fn matched_members(members: usize) -> String {
    let selections: Vec<String> = (1..=members)
        .map(|member| format!("... on O{member} {{ leaf {{ ... on L{member} {{ v }} }} }}"))
        .collect();
    format!("{{ root {{ {} }} }}", selections.join(" "))
}

fn entity_members_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/entity_members.graphql"))
}

/// Each of `items{members}` from `base` with its `id`, and its `__typename` where `observed`, then
/// `fetches` entity fetches from `extra` at the items, conditioned on their members, the `j`th
/// asking each member's `x{j}`.
fn entity_members_plan(
    fixture: &Fixture,
    members: usize,
    fetches: usize,
    observed: bool,
) -> QueryPlan {
    let typename = if observed { "__typename " } else { "" };
    let names: Vec<String> = (1..=members).map(|member| format!("E{member}")).collect();
    let entry = serde_json::Value::Array(
        names
            .iter()
            .map(|name| {
                serde_json::json!({
                    "kind": "InlineFragment",
                    "typeCondition": name,
                    "selections": entry_fields(&["__typename", "id"])
                })
            })
            .collect(),
    );
    let mut nodes = vec![root_fetch(
        "base",
        &format!("{{ items{members} {{ {typename}id }} }}"),
    )];
    for field in 1..=fetches {
        let mut fetch = entity_fetch("extra", "E1", serde_json::Value::Null, "id");
        fetch["Fetch"]["requires"] = entry.clone();
        let asked: Vec<String> = names
            .iter()
            .map(|name| format!("... on {name} {{ x{field} }}"))
            .collect();
        fetch["Fetch"]["operation_document"] = serde_json::json!(format!(
            "query($representations: [_Any!]!) {{ _entities(representations: $representations) \
             {{ {} }} }}",
            asked.join(" ")
        ));
        nodes.push(flatten(
            serde_json::json!([
                { "Key": [format!("items{members}"), null] },
                { "AnyIndex": names }
            ]),
            fetch,
        ));
    }
    plan_of(fixture, sequence(nodes))
}

/// Each member's `x1` to `x{fetches}` at `items{members}`.
fn entity_members_operation(members: usize, fetches: usize) -> String {
    let fields: Vec<String> = (1..=fetches).map(|field| format!("x{field}")).collect();
    let selections: Vec<String> = (1..=members)
        .map(|member| format!("... on E{member} {{ {} }}", fields.join(" ")))
        .collect();
    format!("{{ items{members} {{ {} }} }}", selections.join(" "))
}

// The planner's shape for an interface's entities: every member's fields fetched by entity
// fetches at the items, their path conditioned on the members, which reads each item's
// `__typename`. Fetched with the items, the plan is complete; without it, the conditions read a
// `__typename` no fetch wrote.
//
// Controls of preserved behaviour, whose work `work_grows_with_the_plans_syntax` reads.
#[test]
fn entity_fetches_at_an_interfaces_members_read_their_typename() {
    let fixture = entity_members_fixture();
    for (members, fetches) in [(8, 1), (16, 2)] {
        let operation = entity_members_operation(members, fetches);
        fixture
            .check(
                &entity_members_plan(&fixture, members, fetches, true),
                &operation,
            )
            .unwrap();
        let error = fixture.rejection(
            &entity_members_plan(&fixture, members, fetches, false),
            &operation,
        );
        assert!(error.contains(TYPENAME_NOT_FETCHED), "{error}");
    }
}

/// A family's fixture, plan and client operation at `size`, the plan's neighbour when `neighbour`.
fn family_input(family: &str, size: usize, neighbour: bool) -> (Fixture, QueryPlan, String) {
    match family {
        "covariant" => {
            let fixture = covariant_fixture();
            let leaf = if neighbour {
                "... on A { leaf }"
            } else {
                "leaf"
            };
            let plan = covariant_chain(&fixture, size, leaf);
            (fixture, plan, nexts(size, "leaf"))
        }
        "complementary" => {
            let fixture = complementary_fixture();
            let plan = complementary_chain(&fixture, size, neighbour.then_some(size / 2));
            (fixture, plan, complementary_operation(size))
        }
        "pairs" => {
            let fixture = independent_pairs_fixture();
            let plan = independent_pairs_chain(&fixture, size, neighbour.then_some(size / 2));
            (fixture, plan, independent_pairs_operation(size))
        }
        "fragments" => {
            let fixture = member_fragments_fixture();
            let fetched = member_fragments(size, neighbour.then_some(size / 2 + 1));
            let plan = plan_of(&fixture, root_fetch("chain", &fetched));
            (fixture, plan, member_fragments(size, None))
        }
        "width" => {
            let fixture = member_fragments_fixture();
            let fetched = member_chains(size, neighbour.then_some(5));
            let plan = plan_of(&fixture, root_fetch("chain", &fetched));
            (fixture, plan, member_chains(size, None))
        }
        "entity members" => {
            let fixture = entity_members_fixture();
            let plan = entity_members_plan(&fixture, size, 2, !neighbour);
            (fixture, plan, entity_members_operation(size, 2))
        }
        "entity fetches" => {
            let fixture = entity_members_fixture();
            let plan = entity_members_plan(&fixture, 8, size, !neighbour);
            (fixture, plan, entity_members_operation(8, size))
        }
        other => panic!("no family {other}"),
    }
}

/// The verdict on a plan, with the work it took.
fn family_check(
    fixture: &Fixture,
    plan: &QueryPlan,
    operation: &str,
) -> (Result<(), ComparisonError>, Work) {
    super::check_plan_counted(
        &fixture.supergraph_schema,
        &fixture.subgraphs,
        &fixture.operation(operation),
        plan,
        Default::default(),
    )
}

/// The selections the input writes: those of every fetch's operation in a plan, and the client
/// operation's, counted on their documents and never by the check, so that the check's own
/// traversals cannot inflate what its work is measured against.
fn input_syntax(fixture: &Fixture, plan: &QueryPlan, operation: &str) -> u64 {
    fn selections(list: &[Selection]) -> u64 {
        list.iter()
            .map(|selection| {
                1 + match selection {
                    Selection::Field(field) => selections(&field.selection_set.selections),
                    Selection::InlineFragment(fragment) => {
                        selections(&fragment.selection_set.selections)
                    }
                    Selection::FragmentSpread(_) => 0,
                }
            })
            .sum()
    }
    fn node(plan_node: &PlanNode) -> u64 {
        match plan_node {
            PlanNode::Fetch(fetch) => {
                let document = fetch.operation_document.as_parsed().expect("parsed fetch");
                let operation = document.operations.get(None).expect("one operation");
                selections(&operation.selection_set.selections)
            }
            PlanNode::Flatten(flatten) => node(&flatten.node),
            PlanNode::Sequence(sequence) => sequence.nodes.iter().map(node).sum(),
            PlanNode::Parallel(parallel) => parallel.nodes.iter().map(node).sum(),
            PlanNode::Defer(defer) => {
                defer.primary.node.as_deref().map_or(0, node)
                    + defer
                        .deferred
                        .iter()
                        .map(|block| block.node.as_deref().map_or(0, node))
                        .sum::<u64>()
            }
            PlanNode::Condition(condition) => {
                condition.if_clause.as_deref().map_or(0, node)
                    + condition.else_clause.as_deref().map_or(0, node)
            }
        }
    }
    let plan_syntax = match &plan.node {
        Some(TopLevelPlanNode::Fetch(fetch)) => node(&PlanNode::Fetch(fetch.clone())),
        Some(TopLevelPlanNode::Sequence(sequence)) => node(&PlanNode::Sequence(sequence.clone())),
        Some(_) => unimplemented!("not needed by the families"),
        None => 0,
    };
    let client = fixture.operation(operation);
    let client = client.operations.get(None).expect("one operation");
    plan_syntax + selections(&client.selection_set.selections)
}

/// The loops and the storage the growth laws bound, by name, read from a check's work.
fn bounded_accounts(work: &Work) -> Vec<(&'static str, u64)> {
    vec![
        ("selections visited", work.selections_visited.get()),
        ("contributions recorded", work.contributions_recorded.get()),
        ("literals", work.literals.get()),
        ("set words", work.set_words.get()),
        ("interned words", work.interned_words.get()),
        ("set words stored", work.set_words_stored.get()),
        ("clause comparisons", work.clause_comparisons.get()),
        ("residual clauses", work.residual_clauses.get()),
        (
            "largest region's literals",
            work.largest_region_literals.get(),
        ),
        ("copies visited", work.copies_visited.get()),
        ("obligations", work.obligations.get()),
        ("admissibility reads", work.admissibility_reads.get()),
        ("projections", work.projections.get()),
        ("projection support", work.projection_support.get()),
        ("projection key width", work.projection_key_width.get()),
        ("support literals", work.support_literals.get()),
        ("demand entries", work.demand_entries.get()),
        ("demand key width", work.demand_key_width.get()),
        ("call comparisons", work.call_comparisons.get()),
        ("guard literals", work.guard_literals.get()),
    ]
}

// The work a check takes grows as its input's syntax does, whatever the configurations do. Each
// family is checked at a depth and at twice it, accepted and with its rejected neighbour, and
// every loop's count and every storage account grows by no more than half again what the
// selections the plan and the client write grow by, counted on their documents: the covariant
// chain's and the member fragments' linearly, the other two's quadratically. The configurations
// grow exponentially in the first three: a check enumerating them, carrying a product of
// independent pairs or ancestors, re-traversing what it constructs as many times as the family is
// deep, or comparing every demand with every other, fails here; traversing it a constant number of
// times more does not, since a ratio does not see a constant factor. The member fragments supply
// one key under `members × implementations` guards, one region linear in the syntax: a
// simplification comparing every pair of its clauses fails here; and the entity fetches repeat an
// entity fetch at an interface's members. No region is ever more than two clauses once simplified,
// and no obligation keeps more than a handful of variables, however deep the family. Every account
// that grows too fast is named.
#[test]
fn work_grows_with_the_plans_syntax() {
    let mut grown = Vec::new();
    for (family, size) in [
        ("covariant", 32),
        ("complementary", 16),
        ("pairs", 8),
        ("fragments", 16),
        ("entity fetches", 4),
    ] {
        for neighbour in [false, true] {
            let run = |size: usize| {
                let (fixture, plan, operation) = family_input(family, size, neighbour);
                let (verdict, work) = family_check(&fixture, &plan, &operation);
                assert_eq!(verdict.is_ok(), !neighbour, "{family} at {size}");
                (input_syntax(&fixture, &plan, &operation), work)
            };
            let ((small_syntax, small), (large_syntax, large)) = (run(size), run(2 * size));
            let syntax = large_syntax as f64 / small_syntax as f64;
            for ((name, before), (_, after)) in
                std::iter::zip(bounded_accounts(&small), bounded_accounts(&large))
            {
                let growth = after as f64 / before.max(1) as f64;
                if growth > 1.5 * syntax {
                    grown.push(format!(
                        "{family}{}: {name} grew {growth:.2}× ({before} to {after}) from size \
                         {size} to {}, the input's syntax {syntax:.2}×",
                        if neighbour { " neighbour" } else { "" },
                        2 * size
                    ));
                }
            }
            assert!(
                large.region_clauses_max.get() <= 2,
                "{family}: a region of {} clauses",
                large.region_clauses_max.get()
            );
            assert!(
                large.projection_support_max.get() <= 5,
                "{family}: an obligation over {} variables",
                large.projection_support_max.get()
            );
        }
    }
    assert!(grown.is_empty(), "{}", grown.join("\n"));
}

/// The accounts of a region's simplification, by name.
fn simplification_accounts(work: &Work) -> Vec<(&'static str, u64)> {
    vec![
        ("clause comparisons", work.clause_comparisons.get()),
        ("clauses merged", work.clauses_merged.get()),
        ("bucket literals", work.bucket_literals.get()),
        ("bucket storage", work.bucket_storage.get()),
        ("joined words", work.joined_words.get()),
    ]
}

// A region's simplification does the work of its literals times its widest clause's: each clause
// is bucketed once per variable it holds, on its other literals, and the values a bucket joins
// are joined in one pass. The member chains widen the guards supplying `v` by a literal per level
// of fragments between the members and `v`, `depth + 2` literals; the entity members supply each
// item's `__typename` and `id` under a guard per member of the interface, a literal wide, and
// every member's fields are asked through them. Doubling either grows the simplification accounts
// by no more than half again what the input's syntax grows by, times what the widest clause does.
#[test]
fn simplification_work_grows_with_the_literals_it_joins() {
    // Each family, the size it is read at and twice it, and its widest clause at a size.
    let chain: fn(usize) -> f64 = |depth| depth as f64 + 2.0;
    let flat: fn(usize) -> f64 = |_| 1.0;
    let families = [("width", 4, chain), ("entity members", 16, flat)];
    // Each account, counted somewhere: a family whose keys hold no literal builds none.
    let mut counted = std::collections::BTreeSet::new();
    for (family, size, width) in families {
        for neighbour in [false, true] {
            let run = |size: usize| {
                let (fixture, plan, operation) = family_input(family, size, neighbour);
                let (verdict, work) = family_check(&fixture, &plan, &operation);
                assert_eq!(verdict.is_ok(), !neighbour, "{family} at {size}");
                (input_syntax(&fixture, &plan, &operation), work)
            };
            let ((small_syntax, small), (large_syntax, large)) = (run(size), run(2 * size));
            let syntax = large_syntax as f64 / small_syntax as f64;
            let widened = width(2 * size) / width(size);
            for ((name, before), (_, after)) in std::iter::zip(
                simplification_accounts(&small),
                simplification_accounts(&large),
            ) {
                if before > 0 {
                    counted.insert(name);
                }
                let growth = after as f64 / before.max(1) as f64;
                assert!(
                    growth <= 1.5 * syntax * widened,
                    "{family}{}: {name} grew {growth:.2}× ({before} to {after}), the input's \
                     syntax {syntax:.2}× and its widest clause {widened:.2}×",
                    if neighbour { " neighbour" } else { "" },
                );
            }
        }
    }
    assert_eq!(
        counted.len(),
        simplification_accounts(&Work::default()).len()
    );
}

// Under each member of `Outer`, `v` is selected under the `leaf`'s implementation of the same
// number, so `v` is supplied under `root = Oᵢ ∧ leaf = Lᵢ` for each member `i`: no two of those
// clauses join, differing on both variables, and none implies another, so every one survives and
// absorption compares each with every one kept before it, `m (m − 1) / 2` pairs for `m` members,
// none absorbed.
#[test]
fn absorption_compares_the_clauses_that_survive_joining() {
    let fixture = member_fragments_fixture();
    for (members, pairs) in [(8, 28), (16, 120)] {
        let operation = matched_members(members);
        let plan = plan_of(&fixture, root_fetch("chain", &operation));
        let (verdict, work) = family_check(&fixture, &plan, &operation);
        verdict.unwrap();
        assert_eq!(work.region_clauses_max.get(), members as u64);
        assert_eq!(work.clause_comparisons_unsuccessful.get(), pairs);
    }
}

// An unconditional demand supplied under four guards `$pᵢ ∧ $qᵢ`, eight literals in all, is not
// covered, and the residual subtracting them leaves is made whole: each guard splits every clause
// left in two, one under `¬$pᵢ`, a literal more, and one under `$pᵢ ∧ ¬$qᵢ`, two more, so the four
// leave 2⁴ = 16 clauses, each guard's literals in eight of them once and in eight twice, 4 × 24 =
// 96 literals in all. No other region of the check is as large, so the largest region recorded is
// that residual.
#[test]
fn the_largest_region_account_records_a_subtractions_residual() {
    let fixture = Fixture::new(include_str!("testdata/deferred_seed.graphql"));
    let fetched = "{ parent { __typename } }";
    let guarded = |index: usize| {
        condition(
            &format!("p{index}"),
            Some(condition(
                &format!("q{index}"),
                Some(root_fetch("first", fetched)),
                None,
            )),
            None,
        )
    };
    let plan = plan_of(&fixture, parallel((1..=4).map(guarded).collect()));
    let operation = declaring_booleans(
        (1..=4).flat_map(|index| [format!("p{index}"), format!("q{index}")]),
        fetched,
    );
    let (verdict, work) = family_check(&fixture, &plan, &operation);
    let error = verdict.unwrap_err().to_string();
    assert!(error.contains("does not select `parent`"), "{error}");
    assert_eq!(work.largest_region_clauses.get(), 16);
    assert_eq!(work.largest_region_literals.get(), 96);
}

/// The covariant chain's supergraph with `padding` objects added that nothing reaches, which
/// extraction puts in the chain's subgraph.
fn padded_chain_fixture(padding: usize) -> Fixture {
    let padding: String = (0..padding)
        .map(|index| {
            format!("\ntype Unreached{index} @join__type(graph: CHAIN) {{\n  pad: Int\n}}\n")
        })
        .collect();
    Fixture::new(&format!(
        "{}{padding}",
        include_str!("testdata/covariant_chain.graphql")
    ))
}

/// The covariant chain fetched and asked to `depth`, its last level's `leaf` written as `leaf`,
/// checked over [`padded_chain_fixture`], and the work the check took.
fn padded_chain_check(padding: usize, depth: usize, leaf: &str) -> Work {
    let fixture = padded_chain_fixture(padding);
    let plan = covariant_chain(&fixture, depth, leaf);
    let (verdict, work) = family_check(&fixture, &plan, &nexts(depth, leaf));
    verdict.unwrap();
    work
}

// A check over the supergraph padded with 200 objects nothing reaches decides the same
// obligations and builds the same sets, each four words wide rather than one. What it materializes
// grows by that width: the words interned and stored. What it reads need not: a predicate may stop
// at its first word, so the words scanned grow by at most the width. And building a set from its
// members visits the members alone, so the padding adds nothing there at any number of
// obligations: a check rebuilding a schema-wide set from its members on every call pays the
// padding once per call, a term growing with the obligations the chain's depth makes.
#[test]
fn schema_padding_is_charged_by_the_width_of_what_is_built() {
    for depth in [8, 16] {
        let (narrow, wide) = (
            padded_chain_check(0, depth, "leaf"),
            padded_chain_check(200, depth, "leaf"),
        );
        assert_eq!(narrow.obligations.get(), wide.obligations.get());
        assert_eq!(narrow.type_sets.get(), wide.type_sets.get());
        for (name, narrow, wide) in [
            (
                "interned words",
                narrow.interned_words.get(),
                wide.interned_words.get(),
            ),
            (
                "set words stored",
                narrow.set_words_stored.get(),
                wide.set_words_stored.get(),
            ),
        ] {
            assert!(narrow > 0, "{name}");
            assert_eq!(wide, 4 * narrow, "{name} at depth {depth}");
        }
        let (narrow_scanned, wide_scanned) = (narrow.set_words.get(), wide.set_words.get());
        assert!(
            0 < narrow_scanned
                && narrow_scanned <= wide_scanned
                && wide_scanned <= 4 * narrow_scanned,
            "set words scanned at depth {depth}: {narrow_scanned} narrow, {wide_scanned} wide"
        );
        assert!(narrow.set_bits.get() > 0);
        assert_eq!(
            wide.set_bits.get(),
            narrow.set_bits.get(),
            "members visited at depth {depth}: the padding added {}",
            wide.set_bits.get() - narrow.set_bits.get()
        );
    }
}

// A subset test reads words until one decides it. Over the padded chain's supergraph, four words
// wide, the set of every object with the absent copy is not within the set of every object, which
// only the last word, holding the absent copy's bit, shows; the reverse holds, read through all
// four; and `A` is not within `B`, decided at the first word.
#[test]
fn a_subset_test_reads_up_to_the_word_that_decides_it() {
    let fixture = padded_chain_fixture(200);
    let algebra = Algebra::new(&fixture.supergraph_schema);
    let every = algebra.every_object();
    let with_absent = algebra.union(every, algebra.absent());
    let (a, b) = (
        algebra.objects([&name!("A")]),
        algebra.objects([&name!("B")]),
    );
    for (left, right, holds, words) in [
        (with_absent, every, false, 4),
        (every, with_absent, true, 4),
        (a, b, false, 1),
    ] {
        let before = algebra.work.set_words.get();
        assert_eq!(algebra.is_subset(left, right), holds);
        assert_eq!(algebra.work.set_words.get() - before, words);
    }
}

// The sharing clause asks which of a subgraph's objects stand for a supergraph object only for the
// objects a contribution is read at, once each: the root `Query`, and the `A` and `B` the chain's
// levels can be. Padding the subgraph with objects nothing reaches adds no examination, a deeper
// chain asks about the same objects again and examines none anew, and a chain asked only for
// `__typename` below the root reaches `Query` alone, which asking for `leaf` there adds to by `A`'s
// and `B`'s candidates: each the object of its name and its interface `I`. Asking for `next` there
// too asks about `A` and `B` again, and examines nothing more.
#[test]
fn sharing_examines_the_objects_a_plan_reaches() {
    let base = padded_chain_check(0, 8, "leaf");
    assert!(base.sharing_candidates.get() > 0);
    assert!(base.sharing_entries.get() > 0);
    for (padding, depth) in [(200, 8), (0, 16), (200, 16)] {
        let work = padded_chain_check(padding, depth, "leaf");
        for (name, base, now) in [
            (
                "candidates examined",
                base.sharing_candidates.get(),
                work.sharing_candidates.get(),
            ),
            (
                "correspondences kept",
                base.sharing_entries.get(),
                work.sharing_entries.get(),
            ),
        ] {
            assert_eq!(now, base, "{name} with {padding} padding at depth {depth}");
        }
    }
    let typename = padded_chain_check(200, 0, "__typename");
    let leaf = padded_chain_check(200, 0, "leaf");
    assert_eq!(
        leaf.sharing_candidates.get() - typename.sharing_candidates.get(),
        4,
        "the candidates `leaf` adds"
    );
    let again = padded_chain_check(200, 0, "leaf next { __typename }");
    assert_eq!(
        again.sharing_candidates.get(),
        leaf.sharing_candidates.get(),
        "the candidates `next` adds"
    );
}

/// The interface-object fixture with `padding` implementations of `I` that no field returns, each as
/// composition writes one that `objects` declares as it declares `B`.
fn interface_object_members_fixture(padding: usize) -> Fixture {
    let padding: String = (0..padding)
        .map(|index| {
            format!(
                "\ntype Unreached{index} implements I @join__implements(graph: OBJECTS, interface: \
                 \"I\") @join__type(graph: OBJECTS, key: \"id\") {{\n  id: ID!\n  name: String\n  \
                 label: String @join__field\n}}\n"
            )
        })
        .collect();
    Fixture::new(&format!(
        "{}{padding}",
        include_str!("testdata/interface_object_members.graphql")
    ))
}

/// `operation` fetched from `objects` and asked as it is written, over
/// [`interface_object_members_fixture`], and the work the check took.
fn interface_object_members_check(padding: usize, operation: &str) -> Work {
    let fixture = interface_object_members_fixture(padding);
    let plan = plan_of(&fixture, root_fetch("objects", operation));
    let (verdict, work) = family_check(&fixture, &plan, operation);
    verdict.unwrap();
    work
}

// Which subgraph objects stand for a supergraph object is decided candidate by candidate, without
// enumerating an interface's implementations. `Query.a` reaches `A`, and `whole`'s interface object
// `I`, standing for every implementation of `I`, is one of `A`'s candidates: adding 200
// implementations of `I` that no field returns reads no more names. A second field asked at `A`
// examines nothing anew, `A`'s standing kept; and reaching `B` through `Query.b` adds `B`'s
// candidates alone, `B` and `I` in each of the two subgraphs, reading the same names however many
// implementations `I` has.
#[test]
fn an_interface_objects_correspondence_reads_no_unreached_implementation() {
    let (a, a_twice, a_and_b) = (
        "{ a { size } }",
        "{ a { size name } }",
        "{ a { size } b { name } }",
    );
    let names = |work: &Work| work.correspondence_names.get();
    let candidates = |work: &Work| work.sharing_candidates.get();
    let base = interface_object_members_check(0, a);
    assert!(candidates(&base) > 0);
    assert!(names(&base) > 0);
    for operation in [a, a_twice, a_and_b] {
        let (narrow, wide) = (
            interface_object_members_check(0, operation),
            interface_object_members_check(200, operation),
        );
        assert_eq!(
            names(&wide),
            names(&narrow),
            "names read for `{operation}` with 200 unreached implementations of `I`"
        );
        assert_eq!(candidates(&wide), candidates(&narrow), "{operation}");
    }
    let (once, twice) = (
        interface_object_members_check(200, a),
        interface_object_members_check(200, a_twice),
    );
    assert_eq!(
        candidates(&twice),
        candidates(&once),
        "a second field at `A`"
    );
    assert_eq!(names(&twice), names(&once), "a second field at `A`");
    let both = interface_object_members_check(200, a_and_b);
    assert_eq!(candidates(&both) - candidates(&once), 4, "`B`'s candidates");
    assert!(names(&both) > names(&once));
}

/// `members` fetches of `parent`, each under its own condition `$bᵢ`, then `members²` under `$b0`
/// and a condition `$cⱼ` of their own, over the deferred seed's supergraph.
fn absorbed_guards_plan(fixture: &Fixture, members: usize) -> QueryPlan {
    let fetch = || Some(root_fetch("first", "{ parent { __typename } }"));
    let singles = (0..members).map(|i| condition(&format!("b{i}"), fetch(), None));
    let pairs = (0..members * members)
        .map(|j| condition("b0", Some(condition(&format!("c{j}"), fetch(), None)), None));
    plan_of(fixture, parallel(singles.chain(pairs).collect()))
}

// A clause another absorbs is compared with the kept clauses up to the one absorbing it, and no
// further. Over the deferred seed, `m` fetches of `parent` under conditions `$bᵢ` of their own and
// `m²` under `$b0` and a `$cⱼ` of their own supply `parent` at the root under `m + m²` guards, the
// one region the check simplifies with more than one clause, and no two of them join: the
// singletons, the most general, are each compared with those kept before them and kept,
// `m (m − 1) / 2` comparisons, none absorbing; then each `$b0 ∧ $cⱼ` is absorbed by `$b0`, kept
// first, at its first comparison, `m²` more. Each kept clause visited is one of those comparisons,
// where a traversal going on past the absorbing clause would visit `m² (m − 1)` more. Beside them,
// joining probes its buckets once for each guard holding `$b0`, the one variable more than one
// guard holds: `m² + 1` probes.
#[test]
fn a_clause_absorbed_is_compared_up_to_the_clause_absorbing_it() {
    let fixture = Fixture::new(include_str!("testdata/deferred_seed.graphql"));
    for members in [8, 16] {
        let plan = absorbed_guards_plan(&fixture, members);
        let operation = declaring_booleans(
            (0..members)
                .map(|i| format!("b{i}"))
                .chain((0..members * members).map(|j| format!("c{j}"))),
            "{ parent { __typename } }",
        );
        let (verdict, work) = family_check(&fixture, &plan, &operation);
        let error = verdict.unwrap_err().to_string();
        assert!(error.contains("does not select `parent`"), "{error}");
        let members = members as u64;
        let (pairs, absorbed) = (members * (members - 1) / 2, members * members);
        assert_eq!(
            work.absorption_visits.get(),
            pairs + absorbed,
            "kept clauses visited at {members}"
        );
        assert_eq!(work.clause_comparisons_unsuccessful.get(), pairs);
        assert_eq!(
            work.clause_comparisons.get(),
            (absorbed + 1) + pairs + absorbed,
            "bucket probes and absorption comparisons at {members}"
        );
    }
}

//==================================================================================================
// Conditional `requires` entries: what an entity fetch sends in each configuration
//
// An entry's `@skip`/`@include` conditions are read on the operation's Boolean variables, and the
// representation an entity fetch sends where it runs is what its admitting entries project there,
// in document order, fields replacing and fragments merging: it must have read only what was
// fetched, under the fields its entries name, and cover what the subgraph demands there through a
// key chosen in each configuration. A test porting a theorem of the model's `Apollo/Tests/
// QueryPlanChecker.lean`, one of the sixty of its `ConditionalRequires` namespace or
// `undeclaredConditionNodeWithSufficientBranchesIsRejected` outside it, names the theorem beside
// it; the rest are the port's own controls. Each reads the world of the planner's
// `conditional_requires_sharing_a_fetch_with_an_unconditional_sibling` and says whether its
// verdict is changed or preserved against the checker that grouped the entries by type condition
// and matched each with an entity case.

fn conditional_requires_fixture() -> Fixture {
    Fixture::new(include_str!("testdata/conditional_requires.graphql"))
}

/// The variables a document reads, in the order it first reads each.
fn read_variables(document: &str) -> Vec<String> {
    let mut variables: Vec<String> = Vec::new();
    for (at, _) in document.match_indices('$') {
        let name: String = document[at + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && !variables.contains(&name) {
            variables.push(name);
        }
    }
    variables
}

/// A selection set as an operation declaring, as a `Boolean!`, each variable it reads.
fn declaring_read(selections: &str) -> String {
    let variables = read_variables(selections);
    if variables.is_empty() {
        return selections.to_string();
    }
    let declared: Vec<String> = variables
        .iter()
        .map(|variable| format!("${variable}: Boolean!"))
        .collect();
    format!("query({}) {selections}", declared.join(", "))
}

/// A field of a `requires` entry, with what is selected under it.
fn input(name: &str, below: Vec<serde_json::Value>) -> serde_json::Value {
    let mut field = serde_json::json!({ "kind": "Field", "name": name });
    if !below.is_empty() {
        field["selections"] = serde_json::Value::Array(below);
    }
    field
}

/// A field of a `requires` entry read under an alias.
fn aliased_input(alias: &str, name: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "Field", "alias": alias, "name": name })
}

/// A `requires` inline fragment, on `type_condition` or on none, under `conditions`.
fn entry(
    type_condition: Option<&str>,
    conditions: Vec<serde_json::Value>,
    selections: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut fragment = serde_json::json!({ "kind": "InlineFragment", "selections": selections });
    if let Some(type_condition) = type_condition {
        fragment["typeCondition"] = type_condition.into();
    }
    if !conditions.is_empty() {
        fragment["conditions"] = serde_json::Value::Array(conditions);
    }
    fragment
}

/// An entry on `T`.
fn on_t(
    conditions: Vec<serde_json::Value>,
    selections: Vec<serde_json::Value>,
) -> serde_json::Value {
    entry(Some("T"), conditions, selections)
}

/// `@include` or `@skip` on a variable, or on a literal.
fn include(variable: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "include", "if": variable })
}

fn skip(variable: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "skip", "if": variable })
}

fn include_literal(value: bool) -> serde_json::Value {
    serde_json::json!({ "kind": "include", "if": value })
}

fn skip_literal(value: bool) -> serde_json::Value {
    serde_json::json!({ "kind": "skip", "if": value })
}

/// `__typename` and `id`: what the key of `T` is read from.
fn key_inputs() -> Vec<serde_json::Value> {
    vec![input("__typename", vec![]), input("id", vec![])]
}

/// The key inputs followed by `more`.
fn key_inputs_and(more: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut inputs = key_inputs();
    inputs.extend(more);
    inputs
}

/// The variables a list of entries' conditions name.
fn entry_variables(entries: &[serde_json::Value], out: &mut Vec<String>) {
    for entry in entries {
        if let Some(conditions) = entry["conditions"].as_array() {
            for condition in conditions {
                if let Some(variable) = condition["if"].as_str()
                    && !out.iter().any(|known| known == variable)
                {
                    out.push(variable.to_string());
                }
            }
        }
        if let Some(below) = entry["selections"].as_array() {
            entry_variables(below, out);
        }
    }
}

/// An entity fetch's plan in the conditional world: `Subgraph1` fetches `fetched` on each `parent`,
/// then `Subgraph2` is sent each as `entries`, under `rewrites`, and asked for `cases` under
/// `_entities`, `... on T { selected }` where none are given. `condition` runs the second fetch
/// under one branch of a condition node on a variable.
struct Sent<'a> {
    parent: &'a str,
    fetched: &'a str,
    entries: Vec<serde_json::Value>,
    cases: Option<&'a str>,
    rewrites: serde_json::Value,
    condition: Option<(&'a str, bool)>,
    selected: &'a str,
}

impl<'a> Sent<'a> {
    fn new(fetched: &'a str, entries: Vec<serde_json::Value>, selected: &'a str) -> Self {
        Sent {
            parent: "t",
            fetched,
            entries,
            cases: None,
            rewrites: serde_json::json!([]),
            condition: None,
            selected,
        }
    }

    fn cases(self, cases: &'a str) -> Self {
        Sent {
            cases: Some(cases),
            ..self
        }
    }

    fn rewrites(self, rewrites: serde_json::Value) -> Self {
        Sent { rewrites, ..self }
    }

    fn parent(self, parent: &'a str) -> Self {
        Sent { parent, ..self }
    }

    fn under(self, variable: &'a str, value: bool) -> Self {
        Sent {
            condition: Some((variable, value)),
            ..self
        }
    }

    fn entities(&self) -> String {
        match self.cases {
            Some(cases) => cases.to_string(),
            None => format!("... on T {{ {} }}", self.selected),
        }
    }

    fn plan(&self, fixture: &Fixture) -> QueryPlan {
        let parent = self.parent;
        let fetched = root_fetch(
            "Subgraph1",
            &declaring_read(&format!("{{ {parent} {{ {} }} }}", self.fetched)),
        );
        let entities = self.entities();
        let declared: String = read_variables(&entities)
            .iter()
            .map(|variable| format!(", ${variable}: Boolean!"))
            .collect();
        let sent = serde_json::json!({ "Fetch": {
            "subgraph_name": "Subgraph2",
            "variable_usages": [],
            "requires": self.entries,
            "operation_document": format!(
                "query($representations: [_Any!]!{declared}) {{ \
                 _entities(representations: $representations) {{ {entities} }} }}"
            ),
            "operation_kind": "query",
            "input_rewrites": self.rewrites,
            "output_rewrites": [],
            "context_rewrites": []
        } });
        let sent = flatten(serde_json::json!([{ "Key": [parent, null] }]), sent);
        let sent = match self.condition {
            None => sent,
            Some((variable, true)) => condition(variable, Some(sent), None),
            Some((variable, false)) => condition(variable, None, Some(sent)),
        };
        plan_of(fixture, sequence(vec![fetched, sent]))
    }

    /// The client asking `selected` on each `parent`, declaring every variable the plan reads.
    fn operation(&self) -> String {
        let mut variables = read_variables(self.fetched);
        for variable in read_variables(&self.entities())
            .into_iter()
            .chain(read_variables(self.selected))
        {
            if !variables.contains(&variable) {
                variables.push(variable);
            }
        }
        entry_variables(&self.entries, &mut variables);
        if let Some((variable, _)) = self.condition
            && !variables.iter().any(|known| known == variable)
        {
            variables.push(variable.to_string());
        }
        let operation = format!("{{ {} {{ {} }} }}", self.parent, self.selected);
        if variables.is_empty() {
            operation
        } else {
            declaring_booleans(variables, &operation)
        }
    }

    fn check(&self) -> Result<(), CorrectnessError> {
        let fixture = conditional_requires_fixture();
        fixture.check(&self.plan(&fixture), &self.operation())
    }

    fn rejection(&self) -> String {
        match self.check() {
            Ok(()) => panic!("expected the check to reject this plan"),
            Err(error) => error.to_string(),
        }
    }
}

/// What `Subgraph1` fetches on `t`: its key, and its key with `a` under `$x`.
const KEY_FIELDS: &str = "__typename id";
const KEY_AND_A: &str = "__typename id a";
const GUARDED_A: &str = "__typename id a @include(if: $x)";
/// `b`, which requires `a`, under `$x`, beside an unconditional `c`.
const GUARDED_B: &str = "c b @include(if: $x)";

/// The key unconditionally and `a` under `$x`, in two entries.
fn split_inputs() -> Vec<serde_json::Value> {
    vec![
        on_t(vec![], key_inputs()),
        on_t(vec![include("x")], vec![input("a", vec![])]),
    ]
}

// The planner's plan, transcribed: the unconditional sibling `c` is sent the key alone where
// `$test` is false, and the two entries together send `b`'s `a` where it is true.
//
// The model's `plannerConditionalRequiresSiblingIsAccepted`. A control of preserved behaviour.
#[test]
fn the_planners_conditional_entry_beside_an_unconditional_sibling_is_accepted() {
    Sent::new(
        "__typename id ... on T @include(if: $test) { a }",
        vec![
            on_t(vec![], key_inputs()),
            on_t(
                vec![include("test")],
                vec![input("__typename", vec![]), input("a", vec![])],
            ),
        ],
        "c b @include(if: $test)",
    )
    .check()
    .unwrap();
}

// Without its conditional entry, the planner's plan sends no `a` where `$test` is true.
//
// The model's `plannerConditionalEntryRemovedIsRejected`. A control of preserved behaviour.
#[test]
fn the_planners_plan_without_its_conditional_entry_is_rejected() {
    let error = Sent::new(
        "__typename id ... on T @include(if: $test) { a }",
        vec![on_t(vec![], key_inputs())],
        "c b @include(if: $test)",
    )
    .rejection();
    assert!(
        error.contains("does not hold `a` at `t`, a `T` when $test"),
        "{error}"
    );
}

// `b` is selected unconditionally and `a` was fetched, but the entry reading it is under `$x`:
// where `$x` is false the representation has no `a`, which is named there.
//
// The model's `narrowerEntryLeavesUnconditionalDemandIsRejected`, with its diagnostic. A control of
// changed behaviour.
#[test]
fn a_narrower_entry_leaves_an_unconditional_demand_owed() {
    let error = Sent::new(KEY_AND_A, split_inputs(), "b").rejection();
    assert!(
        error.contains("does not hold `a` at `t`, a `T` when ¬$x"),
        "{error}"
    );
}

// `a` was fetched unconditionally, so a representation holding it everywhere is sound even where
// `$x` excludes `b`.
//
// The model's `coverageWithAvailableSurplusIsAccepted`. A control of preserved behaviour.
#[test]
fn available_surplus_beside_a_conditional_demand_is_accepted() {
    Sent::new(
        KEY_AND_A,
        vec![on_t(vec![], key_inputs_and(vec![input("a", vec![])]))],
        GUARDED_B,
    )
    .check()
    .unwrap();
}

// The same representation where `a` was fetched under `$x` alone: where `$x` is false it reads an
// `a` never fetched.
//
// The model's `coverageWithUnavailableSurplusIsRejected`. A control of changed behaviour.
#[test]
fn unavailable_surplus_is_rejected() {
    let error = Sent::new(
        GUARDED_A,
        vec![on_t(vec![], key_inputs_and(vec![input("a", vec![])]))],
        GUARDED_B,
    )
    .rejection();
    assert!(
        error.contains("the representation reads `a` at `t`, a `T` when ¬$x"),
        "{error}"
    );
    assert!(error.contains(READ_UNFETCHED), "{error}");
}

// A representation holding an available `a` that no demand reads: coverage, not equality.
//
// The model's `unconditionalSurplusIsAccepted`. A control of changed behaviour.
#[test]
fn available_surplus_no_demand_reads_is_accepted() {
    Sent::new(
        KEY_AND_A,
        vec![on_t(vec![], key_inputs_and(vec![input("a", vec![])]))],
        "c",
    )
    .check()
    .unwrap();
}

// Neither entry alone holds both the key and `a`; the representation they project together does.
//
// The model's `splitUnconditionalRepresentationIsAccepted`. A control of changed behaviour.
#[test]
fn a_representation_split_across_unconditional_entries_is_accepted() {
    Sent::new(
        KEY_AND_A,
        vec![
            on_t(vec![], key_inputs()),
            on_t(vec![], vec![input("a", vec![])]),
        ],
        "b",
    )
    .check()
    .unwrap();
}

// The key in an unconditional entry and `a` in one under `$x`: where `$x` holds, only the two
// together cover `b`'s demand.
//
// The model's `splitConditionedRepresentationIsAccepted`. A control of preserved behaviour.
#[test]
fn a_representation_split_across_conditioned_entries_is_accepted() {
    Sent::new(GUARDED_A, split_inputs(), GUARDED_B)
        .check()
        .unwrap();
}

/// `b` under `$x` alone, with nothing else selected.
const CONDITIONAL_B_ONLY: &str = "b @include(if: $x)";

// Where `$x` is false nothing is demanded and nothing sent; where it is true the entry sends `a`
// but no key, and `id` is named there.
//
// The model's `conditionedEntryAloneLacksKeyIsRejected`, with its diagnostic. A control of
// preserved behaviour.
#[test]
fn a_conditioned_entry_without_a_key_is_rejected() {
    let error = Sent::new(
        GUARDED_A,
        vec![on_t(
            vec![include("x")],
            vec![input("__typename", vec![]), input("a", vec![])],
        )],
        CONDITIONAL_B_ONLY,
    )
    .rejection();
    assert!(
        error.contains("does not hold `id` at `t`, a `T` when $x"),
        "{error}"
    );
}

// Adding `id` to the same entry repairs it.
//
// The model's `conditionedEntryWithKeyIsAccepted`. A control of preserved behaviour.
#[test]
fn a_conditioned_entry_with_its_key_is_accepted() {
    Sent::new(
        GUARDED_A,
        vec![on_t(
            vec![include("x")],
            key_inputs_and(vec![input("a", vec![])]),
        )],
        CONDITIONAL_B_ONLY,
    )
    .check()
    .unwrap();
}

/// `Subgraph1` fetches `u`'s two children beside the key.
const KEY_AND_U: &str = "__typename id u { left right }";

/// `u { right }` under a fragment on `T`, then `u { left }`, in one entry, or in the other order.
fn u_right_then_left() -> Vec<serde_json::Value> {
    key_inputs_and(vec![
        on_t(vec![], vec![input("u", vec![input("right", vec![])])]),
        input("u", vec![input("left", vec![])]),
    ])
}

fn u_left_then_right() -> Vec<serde_json::Value> {
    key_inputs_and(vec![
        input("u", vec![input("left", vec![])]),
        on_t(vec![], vec![input("u", vec![input("right", vec![])])]),
    ])
}

// A later field `u` replaces the value an earlier fragment wrote under `u`, its `right` with it,
// though both children were fetched.
//
// The model's `unconditionalLaterFieldReplacementIsRejected`. A control of changed behaviour.
#[test]
fn a_later_field_replaces_what_an_earlier_fragment_wrote() {
    let error = Sent::new(KEY_AND_U, vec![on_t(vec![], u_right_then_left())], "both").rejection();
    assert!(error.contains("does not hold `right` at `t.u`"), "{error}");
}

// The fragment last merges `u.right` into the `u` the field wrote.
//
// The model's `unconditionalLaterFragmentMergeIsAccepted`. A control of preserved behaviour.
#[test]
fn a_later_fragment_merges_into_what_an_earlier_field_wrote() {
    Sent::new(KEY_AND_U, vec![on_t(vec![], u_left_then_right())], "both")
        .check()
        .unwrap();
}

/// A bare `u` beside the key: everything fetched under `u` is copied.
fn copied_u() -> Vec<serde_json::Value> {
    vec![on_t(vec![], key_inputs_and(vec![input("u", vec![])]))]
}

// A bare `u` copies the available `extra: left` beside the demanded `right` with no rename owed for
// it.
//
// The model's `copiedCompositeSurplusAliasIsAccepted`. A control of changed behaviour.
#[test]
fn a_bare_field_copies_surplus_without_a_rename() {
    Sent::new(
        "__typename id u { right extra: left }",
        copied_u(),
        "onRight",
    )
    .check()
    .unwrap();
}

// The same surplus cannot supply the `right` the copy lacks.
//
// The model's `copiedCompositeSurplusMissingDemandIsRejected`. A control of preserved behaviour.
#[test]
fn copied_surplus_cannot_supply_an_omitted_demand() {
    let error = Sent::new("__typename id u { extra: left }", copied_u(), "onRight").rejection();
    assert!(error.contains("does not hold `right` at `t.u`"), "{error}");
}

// A copied value under `right` must be the field `right`.
//
// The model's `copiedCompositeSurplusMisidentifiedDemandIsRejected`. A control of preserved
// behaviour.
#[test]
fn a_copied_value_under_a_demanded_key_must_be_the_demanded_field() {
    let error = Sent::new(
        "__typename id u { right: left extra: left }",
        copied_u(),
        "onRight",
    )
    .rejection();
    assert!(
        error.contains("holds `right` at `t.u`") && error.contains("as `left`"),
        "{error}"
    );
}

/// A renamer at `u` and one of its keys.
fn renaming_below_u(from: &str, to: &str) -> serde_json::Value {
    renaming(&[key("u"), key(from)], to)
}

// A demanded `right` copied under `u` is renamed away by an input rewrite: the subgraph is sent no
// `right`.
//
// The model's `copiedDemandRenamedAwayIsRejected`. A control of preserved behaviour.
#[test]
fn a_demanded_copied_field_renamed_away_is_rejected() {
    let error = Sent::new("__typename id u { right }", copied_u(), "onRight")
        .rewrites(renaming_below_u("right", "gone"))
        .rejection();
    assert!(
        error.contains("the representation sends `right` at `t.u`")
            && error.contains("do not carry the one to the other"),
        "{error}"
    );
}

// A copied `r: right` named back by a rewrite, beside a copied `extra: left` no rewrite names.
//
// The model's `copiedDemandRenamedBackWithSurplusIsAccepted`. A control of changed behaviour.
#[test]
fn a_demanded_copied_field_renamed_back_beside_surplus_is_accepted() {
    Sent::new(
        "__typename id u { r: right extra: left }",
        copied_u(),
        "onRight",
    )
    .rewrites(renaming_below_u("r", "right"))
    .check()
    .unwrap();
}

// The fetch runs where `$x` is false, and `onRight`'s demand is active only where it is true: the
// `right` renamed away is owed nowhere the fetch runs.
//
// The model's `copiedInactiveDemandAddsNoRewriteObligation`. A control of changed behaviour.
#[test]
fn an_inactive_demand_owes_no_rename() {
    Sent::new("__typename id u { right }", copied_u(), "c @skip(if: $x)")
        .cases("... on T { c onRight @include(if: $x) }")
        .rewrites(renaming_below_u("right", "gone"))
        .under("x", false)
        .check()
        .unwrap();
}

/// `u { right }` under a fragment, then `u { left }`, beside the key.
fn overwritten_inputs() -> Vec<serde_json::Value> {
    vec![on_t(vec![], u_right_then_left())]
}

// The later `u { left }` covers `onLeft`, but the earlier `u.right` it overwrites was read all the
// same, and never fetched.
//
// The model's `overwrittenUnfetchedReadIsRejected`. A control of preserved behaviour.
#[test]
fn an_overwritten_read_of_an_unfetched_value_is_rejected() {
    let error = Sent::new("__typename id u { left }", overwritten_inputs(), "onLeft").rejection();
    assert!(
        error.contains("the representation reads `right` at `t.u`")
            && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// Fetching the overwritten `right` repairs it.
//
// The model's `overwrittenFetchedReadIsAccepted`. A control of changed behaviour.
#[test]
fn an_overwritten_read_of_a_fetched_value_is_accepted() {
    Sent::new(KEY_AND_U, overwritten_inputs(), "onLeft")
        .check()
        .unwrap();
}

/// `onLeft` unconditionally and `onRight` under `$x`.
const PROJECTION_DEMAND: &str = "onLeft onRight @include(if: $x)";

// Where `$x` holds, the later field `u { left }` replaces the conditioned fragment's `u.right`,
// which `onRight` demands there: `right` is named at `$x`.
//
// The model's `laterFieldReplacesConditionedProjectionIsRejected`, with its diagnostic. A control
// of changed behaviour.
#[test]
fn a_later_field_replaces_a_conditioned_fragments_value() {
    let error = Sent::new(
        KEY_AND_U,
        vec![on_t(
            vec![],
            key_inputs_and(vec![
                on_t(
                    vec![include("x")],
                    vec![input("u", vec![input("right", vec![])])],
                ),
                input("u", vec![input("left", vec![])]),
            ]),
        )],
        PROJECTION_DEMAND,
    )
    .rejection();
    assert!(
        error.contains("does not hold `right` at `t.u`, a `U` when `t` is `T` ∧ $x"),
        "{error}"
    );
}

// The conditioned fragment last merges `u.right` where `$x` holds and adds nothing elsewhere.
//
// The model's `laterConditionedFragmentMergesProjectionIsAccepted`. A control of preserved
// behaviour.
#[test]
fn a_later_conditioned_fragment_merges_into_a_fields_value() {
    Sent::new(
        KEY_AND_U,
        vec![on_t(
            vec![],
            key_inputs_and(vec![
                input("u", vec![input("left", vec![])]),
                on_t(
                    vec![include("x")],
                    vec![input("u", vec![input("right", vec![])])],
                ),
            ]),
        )],
        PROJECTION_DEMAND,
    )
    .check()
    .unwrap();
}

/// `u { r: right }`, read and then replaced by `u { left }`, beside the key.
fn overwritten_alias() -> Vec<serde_json::Value> {
    vec![on_t(
        vec![],
        key_inputs_and(vec![
            input("u", vec![aliased_input("r", "right")]),
            input("u", vec![input("left", vec![])]),
        ]),
    )]
}

// An alias read and then overwritten does not survive: with both inputs fetched and only `u.left`
// demanded, no rewrite is owed for it.
//
// A port control of the model's rewrite agreement on surviving explicit selections. A control of
// changed behaviour.
#[test]
fn an_overwritten_alias_owes_no_rename() {
    Sent::new(
        "__typename id u { left r: right }",
        overwritten_alias(),
        "onLeft",
    )
    .check()
    .unwrap();
}

// The same with the alias's input omitted: the read of it is still owed.
//
// Its neighbour, beside the model's `overwrittenUnfetchedReadIsRejected`. A control of preserved
// behaviour.
#[test]
fn an_overwritten_alias_owes_its_read() {
    let error = Sent::new("__typename id u { left }", overwritten_alias(), "onLeft").rejection();
    assert!(
        error.contains("the representation reads `r` at `t.u`") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

// A later fragment's bare `u` writes `u.r` again as copied surplus: the explicit alias the earlier
// field wrote there is no longer the surviving write, so no rename is owed for it.
//
// A port control of the model's `mergeRepresentation`, whose explicit selections a later fragment
// removes where it writes a value, not where it writes an explicit selection. A control of changed
// behaviour.
#[test]
fn a_later_fragments_bare_copy_clears_an_earlier_explicit_alias() {
    Sent::new(
        "__typename id u { left r: right }",
        vec![on_t(
            vec![],
            key_inputs_and(vec![
                input("u", vec![aliased_input("r", "right")]),
                on_t(vec![], vec![input("u", vec![])]),
            ]),
        )],
        "onLeft",
    )
    .check()
    .unwrap();
}

// A field inside a fragment replaces only what that fragment wrote: the enclosing `u.right`
// survives the fragment's own `u { right } u { left }`, and the merge leaves both children.
//
// A port control of the scope a field clears, a fragment's projection starting empty. A control of
// preserved behaviour.
#[test]
fn a_fragments_own_replacement_leaves_the_enclosing_value() {
    Sent::new(
        KEY_AND_U,
        vec![on_t(
            vec![],
            key_inputs_and(vec![
                input("u", vec![input("right", vec![])]),
                on_t(
                    vec![],
                    vec![
                        input("u", vec![input("right", vec![])]),
                        input("u", vec![input("left", vec![])]),
                    ],
                ),
            ]),
        )],
        "both",
    )
    .check()
    .unwrap();
}

// An entry on `U` sends each `t.u` a representation, but `Subgraph2` resolves no `U` entity: no key
// of it can be chosen, and the plan is refused with the object named.
//
// A port control of the model's `RepresentationCovers` at a runtime type `entityTypesAt` gives no
// entity type: a non-empty representation there covers nothing.
#[test]
fn a_representation_of_an_object_the_subgraph_resolves_no_entity_of_is_rejected() {
    let fixture = conditional_requires_fixture();
    let mut sent = entity_fetch("Subgraph2", "T", entry_fields(&["left"]), "c");
    sent["Fetch"]["requires"][0]["typeCondition"] = "U".into();
    let plan = plan_of(
        &fixture,
        sequence(vec![
            root_fetch("Subgraph1", "{ t { __typename id u { left } } }"),
            flatten(
                serde_json::json!([{ "Key": ["t", null] }, { "Key": ["u", null] }]),
                sent,
            ),
        ]),
    );
    let error = fixture.rejection(&plan, "{ t { u { left } } }");
    assert!(
        error.contains("is of a `U`, which the subgraph resolves no entity of"),
        "{error}"
    );
}

/// The key and `a` under `$x`, and again under `¬$x`.
fn opposite_inputs() -> Vec<serde_json::Value> {
    vec![
        on_t(vec![include("x")], key_inputs_and(vec![input("a", vec![])])),
        on_t(vec![skip("x")], key_inputs_and(vec![input("a", vec![])])),
    ]
}

// Opposite entries on `$x` supply the unconditional `b` in both configurations.
//
// The model's `complementaryEntriesCoverBothConfigurationsIsAccepted`. A control of preserved
// behaviour.
#[test]
fn complementary_entries_cover_both_configurations() {
    Sent::new(KEY_AND_A, opposite_inputs(), "b")
        .check()
        .unwrap();
}

// Without the `@skip` entry nothing is sent where `$x` is false, and `b` is demanded there.
//
// The model's `complementaryEntryRemovedIsRejected`. A control of changed behaviour.
#[test]
fn a_removed_complementary_entry_leaves_its_configuration_uncovered() {
    let error = Sent::new(
        KEY_AND_A,
        vec![on_t(
            vec![include("x")],
            key_inputs_and(vec![input("a", vec![])]),
        )],
        "b",
    )
    .rejection();
    assert!(error.contains("¬$x"), "{error}");
}

// `V` has two keys: the representation sends `id` where `$x` holds and `id2` where it does not, so
// each configuration resolves a key and no one key covers both.
//
// The model's `complementaryEntriesMayResolveDifferentKeysIsAccepted`. A control of changed
// behaviour.
#[test]
fn complementary_entries_may_resolve_different_keys() {
    let entries = vec![
        entry(
            Some("V"),
            vec![include("x")],
            vec![input("__typename", vec![]), input("id", vec![])],
        ),
        entry(
            Some("V"),
            vec![skip("x")],
            vec![input("__typename", vec![]), input("id2", vec![])],
        ),
    ];
    Sent::new("__typename id id2", entries, "c")
        .parent("v")
        .cases("... on V { c }")
        .check()
        .unwrap();
}

// The same with the `@skip` entry's `id2` dropped: where `$x` is false neither key is sent, and
// every key's failure there is named.
//
// A neighbour of the model's `complementaryEntriesMayResolveDifferentKeysIsAccepted`. A control of
// changed behaviour.
#[test]
fn a_configuration_no_key_covers_is_rejected_with_each_keys_failure() {
    let entries = vec![
        entry(
            Some("V"),
            vec![include("x")],
            vec![input("__typename", vec![]), input("id", vec![])],
        ),
        entry(
            Some("V"),
            vec![skip("x")],
            vec![input("__typename", vec![])],
        ),
    ];
    let error = Sent::new("__typename id id2", entries, "c")
        .parent("v")
        .cases("... on V { c }")
        .rejection();
    assert!(
        error.contains("through none of its resolvable keys when `v` is `V` ∧ ¬$x"),
        "{error}"
    );
    assert!(
        error.contains("@key(id)") && error.contains("@key(id2)"),
        "{error}"
    );
}

// An entry with no type condition admits the object it is read at.
//
// The model's `untypedUnconditionalEntryIsAccepted`. A control of changed behaviour.
#[test]
fn an_untyped_entry_admits_the_object() {
    Sent::new(KEY_FIELDS, vec![entry(None, vec![], key_inputs())], "c")
        .check()
        .unwrap();
}

// An entry on the interface `I` projects `a` into the same `T` representation as the object
// entry's key.
//
// The model's `overlappingInterfaceAndObjectEntriesAreAccepted`. A control of changed behaviour.
#[test]
fn an_interface_entry_projects_beside_an_object_entry() {
    Sent::new(
        KEY_AND_A,
        vec![
            on_t(vec![], key_inputs()),
            entry(Some("I"), vec![], vec![input("a", vec![])]),
        ],
        "b",
    )
    .check()
    .unwrap();
}

// The entity fragment is under `$x`, but the unconditional representation still reads `a` where
// `$x` is false, where it was not fetched.
//
// The model's `entityFragmentCannotHideUnreadSurplusIsRejected`. A control of changed behaviour.
#[test]
fn an_entity_fragments_condition_cannot_hide_an_unread_surplus() {
    let error = Sent::new(
        GUARDED_A,
        vec![on_t(vec![], key_inputs_and(vec![input("a", vec![])]))],
        "b @include(if: $x)",
    )
    .cases("... on T @include(if: $x) { b }")
    .rejection();
    assert!(
        error.contains("the representation reads `a` at `t`, a `T` when ¬$x"),
        "{error}"
    );
}

// The fragment under `@include(if: false)` owes no `a`; its sibling's key is still sent.
//
// The model's `entityFragmentLiteralFalseIsAccepted`. A control of changed behaviour.
#[test]
fn an_entity_fragment_under_a_false_literal_owes_nothing() {
    Sent::new(
        KEY_FIELDS,
        vec![on_t(vec![], key_inputs()), on_t(vec![], key_inputs())],
        "c",
    )
    .cases("... on T { c } ... on T @include(if: false) { b }")
    .check()
    .unwrap();
}

// Two fragments on `T` are active together: `a` and `e` are demanded together, and one
// representation holds both.
//
// The model's `sameTypeFragmentDemandsTogetherAreAccepted`. A control of preserved behaviour.
#[test]
fn same_type_entity_fragments_are_demanded_together() {
    Sent::new(
        "__typename id a e",
        vec![on_t(
            vec![],
            key_inputs_and(vec![input("a", vec![]), input("e", vec![])]),
        )],
        "b d",
    )
    .cases("... on T { b } ... on T { d }")
    .check()
    .unwrap();
}

// The second fragment's `d` demands `e`, which the representation lacks.
//
// The model's `sameTypeFragmentDemandMissingIsRejected`. A control of preserved behaviour.
#[test]
fn a_second_same_type_entity_fragments_demand_stays_owed() {
    let error = Sent::new(
        "__typename id a e",
        vec![on_t(vec![], key_inputs_and(vec![input("a", vec![])]))],
        "b d",
    )
    .cases("... on T { b } ... on T { d }")
    .rejection();
    assert!(error.contains("does not hold `e` at `t`"), "{error}");
}

/// The key, and `a` in an entry under one literal condition; `c`, and `b` under the matching
/// directive, selected; `a` fetched or not.
fn literal_sent(
    condition: serde_json::Value,
    directive: &'static str,
    fetched_a: bool,
) -> Result<(), CorrectnessError> {
    let selected = format!("c b {directive}");
    Sent::new(
        if fetched_a { KEY_AND_A } else { KEY_FIELDS },
        vec![
            on_t(vec![], key_inputs()),
            on_t(vec![condition], vec![input("a", vec![])]),
        ],
        &selected,
    )
    .check()
}

// `@include(if: true)` folds to no condition, and `a` is sent.
//
// The model's `includeTrueReadsTheEntryIsAccepted`. A control of preserved behaviour.
#[test]
fn include_true_reads_its_entry() {
    literal_sent(include_literal(true), "@include(if: true)", true).unwrap();
}

// `@include(if: false)` excludes the entry, so the `a` never fetched is never read, and the
// unconditional key serves `c`.
//
// The model's `includeFalseExcludesTheEntryIsAccepted`. A control of preserved behaviour.
#[test]
fn include_false_excludes_its_entry() {
    literal_sent(include_literal(false), "@include(if: false)", false).unwrap();
}

// `@skip(if: false)` folds to no condition, and `a` is sent.
//
// The model's `skipFalseReadsTheEntryIsAccepted`. A control of preserved behaviour.
#[test]
fn skip_false_reads_its_entry() {
    literal_sent(skip_literal(false), "@skip(if: false)", true).unwrap();
}

// `@skip(if: true)` excludes the entry, which is not an empty condition list admitting it.
//
// The model's `skipTrueExcludesTheEntryIsAccepted`. A control of preserved behaviour.
#[test]
fn skip_true_excludes_its_entry() {
    literal_sent(skip_literal(true), "@skip(if: true)", false).unwrap();
}

// Every entry excluded by a literal: the fetch sends nothing, so it serves no `c`, as it would if
// it had become a root fetch.
//
// The model's `allLiteralExcludedEntriesSendNothingIsRejected`. A control of changed behaviour.
#[test]
fn entries_all_excluded_send_nothing() {
    let error = Sent::new(
        KEY_FIELDS,
        vec![on_t(vec![include_literal(false)], key_inputs())],
        "c",
    )
    .rejection();
    assert!(
        error.contains("does not hold `__typename` at `t`"),
        "{error}"
    );
}

// Excluded entries still make the fetch an entity fetch, which must select `_entities`; with no
// entries at all the same root operation is a root fetch, and fine.
//
// The model's `allLiteralExcludedEntriesKeepEntityKindIsRejected`. A control of preserved
// behaviour.
#[test]
fn entries_all_excluded_keep_the_fetch_an_entity_fetch() {
    let fixture = conditional_requires_fixture();
    let plan = |requires: serde_json::Value| {
        plan_of(
            &fixture,
            sequence(vec![
                root_fetch("Subgraph1", "{ t { __typename id } }"),
                with(
                    root_fetch("Subgraph2", "{ _service { sdl } }"),
                    "requires",
                    requires,
                ),
            ]),
        )
    };
    let error = fixture.rejection(
        &plan(serde_json::json!([on_t(
            vec![include_literal(false)],
            key_inputs()
        )])),
        "{ t { id } }",
    );
    assert!(
        error.contains("an entity fetch must select exactly one root field, `_entities`"),
        "{error}"
    );
    fixture
        .check(&plan(serde_json::json!([])), "{ t { id } }")
        .unwrap();
}

/// `u` with a fragment under `@include(if: false)` below it, beside the key.
fn u_with_an_excluded_fragment() -> Vec<serde_json::Value> {
    vec![on_t(
        vec![],
        key_inputs_and(vec![input(
            "u",
            vec![entry(
                None,
                vec![include_literal(false)],
                vec![input("right", vec![])],
            )],
        )]),
    )]
}

// Excluding the fragment below `u` leaves `u` itself selected: the available `u` is read, its
// excluded `right` is not.
//
// The model's `nestedLiteralExclusionKeepsContainingFieldIsAccepted`. A control of changed
// behaviour.
#[test]
fn an_excluded_nested_fragment_keeps_its_containing_field() {
    Sent::new(
        "__typename id u { left }",
        u_with_an_excluded_fragment(),
        "c",
    )
    .check()
    .unwrap();
}

// The same entry where `u` was never fetched: its read is owed whatever is excluded below it.
//
// The model's `nestedLiteralExclusionCannotEraseContainingReadIsRejected`. A control of preserved
// behaviour.
#[test]
fn an_excluded_nested_fragment_cannot_erase_its_containing_read() {
    let error = Sent::new(KEY_FIELDS, u_with_an_excluded_fragment(), "c").rejection();
    assert!(
        error.contains("the representation reads `u` at `t`") && error.contains(READ_UNFETCHED),
        "{error}"
    );
}

/// The key, and `a` in an entry under `conditions`; `c` selected and fetched, by an operation
/// declaring no variable.
fn undeclared_sent(conditions: Vec<serde_json::Value>) -> String {
    let fixture = conditional_requires_fixture();
    let sent = Sent::new(
        KEY_FIELDS,
        vec![
            on_t(vec![], key_inputs()),
            on_t(conditions, vec![input("a", vec![])]),
        ],
        "c",
    );
    fixture.rejection(&sent.plan(&fixture), "{ t { c } }")
}

// `$missing` is declared nowhere: `@include` and `@skip` on it do not make the entry unreachable,
// since the router includes it, and the plan is refused with the variable named.
//
// The model's `unresolvedOppositeConditionsAreRefusedByName`. A control whose verdict is preserved
// and whose reason changes: the trunk rejects it for the `a` it reads as undemanded, naming no
// variable.
#[test]
fn an_undeclared_variable_under_opposite_conditions_is_refused_by_name() {
    let error = undeclared_sent(vec![include("missing"), skip("missing")]);
    assert!(
        error.contains("the plan conditions on `$missing`, which the operation does not declare"),
        "{error}"
    );
}

// Folding the false literal beside it does not hide `$missing`.
//
// The model's `literalFalseBesideUnresolvedVariableIsRefusedByName`. A control whose verdict is
// preserved and whose reason changes: the trunk rejects it for the `a` it reads as undemanded,
// naming no variable.
#[test]
fn an_undeclared_variable_beside_a_false_literal_is_refused_by_name() {
    let error = undeclared_sent(vec![include_literal(false), include("missing")]);
    assert!(
        error.contains("the plan conditions on `$missing`, which the operation does not declare"),
        "{error}"
    );
}

// With `$x` declared, `@include` and `@skip` on it exclude the entry in both configurations, so the
// unfetched `a` is never read.
//
// The model's `resolvedOppositeConditionsExcludeTheEntryIsAccepted`. A control of changed
// behaviour.
#[test]
fn opposite_conditions_on_a_declared_variable_exclude_their_entry() {
    Sent::new(
        KEY_FIELDS,
        vec![
            on_t(vec![], key_inputs()),
            on_t(vec![include("x"), skip("x")], vec![input("a", vec![])]),
        ],
        "c",
    )
    .check()
    .unwrap();
}

/// The operation using a `String` as `t`'s argument, and the plan sending the key under
/// `conditions`.
fn string_variable_plan(fixture: &Fixture, conditions: Vec<serde_json::Value>) -> QueryPlan {
    let mut sent = entity_fetch("Subgraph2", "T", serde_json::json!([]), "c");
    sent["Fetch"]["requires"] = serde_json::json!([on_t(conditions, key_inputs())]);
    plan_of(
        fixture,
        sequence(vec![
            root_fetch(
                "Subgraph1",
                "query($s: String) { t(s: $s) { __typename id } }",
            ),
            flatten(serde_json::json!([{ "Key": ["t", null] }]), sent),
        ]),
    )
}

const STRING_VARIABLE: &str = "query($s: String) { t(s: $s) { c } }";

// A `String` the operation uses as an argument is no Boolean: a condition on it is refused with the
// variable named.
//
// A port control of the model's `DeclaresBoolean`: a declared variable of another type is no
// configuration's Boolean. A control of changed behaviour.
#[test]
fn a_condition_on_a_string_variable_is_refused_by_name() {
    let fixture = conditional_requires_fixture();
    let error = fixture.rejection(
        &string_variable_plan(&fixture, vec![include("s")]),
        STRING_VARIABLE,
    );
    assert!(
        error.contains(
            "the plan conditions on `$s`, which the operation does not declare as `Boolean` or \
             `Boolean!`"
        ),
        "{error}"
    );
}

// The same entry with no condition is accepted.
//
// Its neighbour. A control of preserved behaviour.
#[test]
fn the_same_entry_unconditioned_beside_a_string_variable_is_accepted() {
    let fixture = conditional_requires_fixture();
    fixture
        .check(&string_variable_plan(&fixture, vec![]), STRING_VARIABLE)
        .unwrap();
}

/// The key, and `a` under `$x` and, nested, `$y`.
fn nested_inputs() -> Vec<serde_json::Value> {
    vec![
        on_t(vec![], key_inputs()),
        on_t(
            vec![include("x")],
            vec![on_t(vec![include("y")], vec![input("a", vec![])])],
        ),
    ]
}

// The two independent conditions are conjoined: `a` is read only where both hold, and fetched
// there.
//
// The model's `nestedIndependentConditionsAreConjoinedIsAccepted`. A control of preserved
// behaviour.
#[test]
fn nested_conditions_are_conjoined() {
    Sent::new(
        "__typename id ... @include(if: $x) { a @include(if: $y) }",
        nested_inputs(),
        "c ... @include(if: $x) { b @include(if: $y) }",
    )
    .check()
    .unwrap();
}

// `b` is demanded under `$y`, but the outer `$x` excludes its `a` where `$x` is false.
//
// The model's `nestedOuterExclusionCannotCoverYDemandIsRejected`. A control of changed behaviour.
#[test]
fn an_outer_condition_cannot_cover_a_demand_under_the_inner_one() {
    let error = Sent::new(KEY_AND_A, nested_inputs(), "c b @include(if: $y)").rejection();
    assert!(
        error.contains("does not hold `a` at `t`") && error.contains("¬$x") && error.contains("$y"),
        "{error}"
    );
}

// `b` is demanded under `$x`, but the inner `$y` excludes its `a` where `$y` is false.
//
// The model's `nestedInnerExclusionCannotCoverXDemandIsRejected`. A control of changed behaviour.
#[test]
fn an_inner_condition_cannot_cover_a_demand_under_the_outer_one() {
    let error = Sent::new(KEY_AND_A, nested_inputs(), GUARDED_B).rejection();
    assert!(
        error.contains("does not hold `a` at `t`") && error.contains("¬$y"),
        "{error}"
    );
}

// An untyped fragment under `$x` at the top admits `T`, and reads `a` where `b` selects it.
//
// The model's `untypedConditionedTopEntryIsAccepted`. A control of changed behaviour.
#[test]
fn an_untyped_conditioned_entry_admits_the_object() {
    Sent::new(
        GUARDED_A,
        vec![
            on_t(vec![], key_inputs()),
            entry(None, vec![include("x")], vec![input("a", vec![])]),
        ],
        GUARDED_B,
    )
    .check()
    .unwrap();
}

// The same fragment nested in an untyped one keeps its condition and admits the same object.
//
// The model's `untypedConditionedNestedEntryIsAccepted`. A control of changed behaviour.
#[test]
fn an_untyped_conditioned_entry_nested_in_an_untyped_one_admits_the_object() {
    Sent::new(
        GUARDED_A,
        vec![
            on_t(vec![], key_inputs()),
            entry(
                None,
                vec![],
                vec![entry(None, vec![include("x")], vec![input("a", vec![])])],
            ),
        ],
        GUARDED_B,
    )
    .check()
    .unwrap();
}

/// `c` unconditionally, and `b` under `$x` in a fragment of its own.
const C_AND_GUARDED_B_CASES: &str = "... on T { c } ... on T @include(if: $x) { b }";

// The entity fragment's own `@include(if: $x)` owes `a` only where `$x` holds, where its entry and
// the fetch before supply it.
//
// The model's `entityFragmentDirectiveLimitsDemandIsAccepted`. A control of preserved behaviour.
#[test]
fn an_entity_fragments_condition_limits_its_demand() {
    Sent::new(GUARDED_A, split_inputs(), GUARDED_B)
        .cases(C_AND_GUARDED_B_CASES)
        .check()
        .unwrap();
}

// The fragment admits `b` under `$x` where the entry for its `a` is under `$y`: where `$x` holds
// and `$y` does not, `a` is owed and not sent.
//
// The model's `entityFragmentAdmitsConfigurationItsEntryExcludesIsRejected`. A control of changed
// behaviour.
#[test]
fn an_entity_fragment_admitting_what_its_entry_excludes_is_rejected() {
    let error = Sent::new(
        KEY_AND_A,
        vec![
            on_t(vec![], key_inputs()),
            on_t(vec![include("y")], vec![input("a", vec![])]),
        ],
        GUARDED_B,
    )
    .cases(C_AND_GUARDED_B_CASES)
    .rejection();
    assert!(
        error.contains("does not hold `a` at `t`") && error.contains("$x") && error.contains("¬$y"),
        "{error}"
    );
}

// The operation declares `$x` a `Boolean` with a default: the variable is declared, and the
// configurations give it either value, an override of the default included.
//
// The model's `defaultedBooleanPassesTheBoundaryIsAccepted`. A control of changed behaviour.
#[test]
fn a_defaulted_boolean_passes_the_boundary() {
    let fixture = conditional_requires_fixture();
    let sent = Sent::new(
        KEY_FIELDS,
        vec![
            on_t(vec![], key_inputs()),
            on_t(vec![include("x"), skip("x")], vec![input("a", vec![])]),
        ],
        "c",
    );
    fixture
        .check(
            &sent.plan(&fixture),
            "query($x: Boolean = false) { __typename @include(if: $x) t { c } }",
        )
        .unwrap();
}

// The entity fragment's own `@skip(if: $x)` owes `a` where `$x` is false alone, matching its entry
// and the fetch before.
//
// The model's `entityFragmentSkipLimitsDemandIsAccepted`. A control of preserved behaviour.
#[test]
fn an_entity_fragments_skip_limits_its_demand() {
    Sent::new(
        "__typename id a @skip(if: $x)",
        vec![
            on_t(vec![], key_inputs()),
            on_t(vec![skip("x")], vec![input("a", vec![])]),
        ],
        "c b @skip(if: $x)",
    )
    .cases("... on T { c } ... on T @skip(if: $x) { b }")
    .check()
    .unwrap();
}

// Both branches of a condition node on an undeclared `$missing` fetch everything the operation
// asks: the variable alone is refused, by name.
//
// The model's `undeclaredConditionNodeWithSufficientBranchesIsRejected`. A control of changed
// behaviour.
#[test]
fn an_undeclared_condition_node_variable_is_refused_where_both_branches_suffice() {
    let fixture = conditional_requires_fixture();
    let fetch = || Some(root_fetch("Subgraph1", "{ v { id } }"));
    let plan = plan_of(&fixture, condition("missing", fetch(), fetch()));
    let error = fixture.rejection(&plan, "{ v { id } }");
    assert!(
        error.contains("the plan conditions on `$missing`, which the operation does not declare"),
        "{error}"
    );
}

// An `_entities` fragment with no type condition is refused as the fetch's syntax, where a
// `requires` entry may have none; the typed fragment under it, written as a fragment of its own,
// is accepted.
//
// A port control of the model's `entityOperationWellFormed`. A control of preserved behaviour.
#[test]
fn an_untyped_entities_fragment_is_refused_as_syntax() {
    let entries = vec![on_t(vec![], key_inputs())];
    let error = Sent::new(KEY_FIELDS, entries.clone(), "c @include(if: $x)")
        .cases("... @include(if: $x) { ... on T { c } }")
        .rejection();
    assert!(
        error.contains("an `_entities` selection must carry a type condition"),
        "{error}"
    );
    Sent::new(KEY_FIELDS, entries, "c @include(if: $x)")
        .cases("... on T @include(if: $x) { c }")
        .check()
        .unwrap();
}

//--------------------------------------------------------------------------------------------------
// Grounding a type condition against its position, through coverage

/// `media` read under the key as `inputs`, demanded by `field`.
fn media_sent(inputs: Vec<serde_json::Value>, field: &str) -> Result<(), CorrectnessError> {
    Sent::new(
        "__typename id media { __typename id }",
        vec![on_t(vec![], key_inputs_and(vec![input("media", inputs)]))],
        field,
    )
    .check()
}

// A demand naming the whole interface, `media { ... on Media { id } }`, admits every object at that
// position, and `media { id }` covers it.
//
// The model's `conditionNamingThePositionItselfIsMatched`, migrated to coverage. A control of
// preserved behaviour.
#[test]
fn a_demand_naming_the_position_itself_is_covered() {
    media_sent(vec![input("id", vec![])], "forMedia").unwrap();
}

// An `id` sent for every `Media` covers a `Photo`'s demand; a `Video`'s is available surplus.
//
// The model's `broaderEntryCoversNarrowedDemand`, which replaced
// `realNarrowingInTheDemandIsNotMatched`. A control of changed behaviour.
#[test]
fn a_broader_entry_covers_a_narrowed_demand() {
    media_sent(vec![input("id", vec![])], "forPhotos").unwrap();
}

// An `id` sent for a `Photo` alone does not cover a demand on every `Media`: a `Video` still owes
// it.
//
// The model's `narrowedEntryDoesNotCoverBroaderDemand`, which replaced `narrowerEntryIsMatched`. A
// control of preserved behaviour.
#[test]
fn a_narrowed_entry_does_not_cover_a_broader_demand() {
    let error = media_sent(
        vec![entry(Some("Photo"), vec![], vec![input("id", vec![])])],
        "forAll",
    )
    .expect_err("a `Video` owes its `id`")
    .to_string();
    assert!(
        error.contains("does not hold `id` at `t.media`, a `Video`"),
        "{error}"
    );
}

//--------------------------------------------------------------------------------------------------
// What conditional entries cost
//
// The model's growth controls count complete assignments: two for any number of entries over one
// variable, 2, 4 and 8 for one, two and three independent ones. This checker enumerates no
// assignment, so those counts are not its to reach; what it owes is that an entry's conditions add
// the work of reading them and the obligations they guard, never a configuration product. Each test
// below states the work and storage it expects of a family before it runs: the selections and
// literals the projection reads and the regions it writes, exact in the family's size; the
// obligations decided and the most clauses one region held, either the same at every size or
// growing by a constant per entry; and the contributions the representation holds. A product over
// `n` independent variables would make a region or the obligations grow as `2ⁿ`.

/// The conditional world with `fields` more fields of `T`, `f1` to `fN`, which `Subgraph1`
/// resolves and nothing requires.
fn conditional_world_with_fields(fields: usize) -> Fixture {
    let supergraph = include_str!("testdata/conditional_requires.graphql");
    let opening = "type T implements I @join__implements(graph: SUBGRAPH1, interface: \"I\") \
                   @join__type(graph: SUBGRAPH1, key: \"id\") @join__type(graph: SUBGRAPH2, key: \"id\") {\n";
    let added: String = (1..=fields)
        .map(|i| format!("  f{i}: Int @join__field(graph: SUBGRAPH1)\n"))
        .collect();
    assert!(
        supergraph.contains(opening),
        "the fixture's `T` is where it was"
    );
    Fixture::new(&supergraph.replacen(opening, &format!("{opening}{added}"), 1))
}

/// The key, then `n` entries each reading `fields(i)` under `@include(if: variable(i))`.
fn conditioned_entries(
    n: usize,
    variable: impl Fn(usize) -> String,
    fields: impl Fn(usize) -> String,
) -> Vec<serde_json::Value> {
    let mut entries = vec![on_t(vec![], key_inputs())];
    entries.extend(
        (1..=n).map(|i| on_t(vec![include(&variable(i))], vec![input(&fields(i), vec![])])),
    );
    entries
}

fn sent_work(fixture: &Fixture, sent: &Sent<'_>) -> (Result<(), ComparisonError>, Work) {
    family_check(fixture, &sent.plan(fixture), &sent.operation())
}

// Entries over one variable, `n` of them, each reading `a` under `$x`. Expected, before the run:
// the projection reads `2n + 3` selections (the key's fragment and its two fields, each entry's
// fragment and its field) and `n` literals, and the boundary `n` references; it writes `6n + 9`
// regions (three per field read, four for the key's merge, five for the first entry's and six for
// every later one's, which narrows the explicit selection the one before left); it scans nothing,
// no entry reading a composite. Everything else is the same at every `n`: `a`'s regions stay one
// clause, `$x`, so the obligations decided, the most clauses a region held, the most variables an
// obligation kept, and the three contributions the representation holds with their literals.
//
// The work-control counterpart of the model's
// `fixedVariableEntriesKeepTwoConfigurationsIsAccepted`, at its three and six entries and beyond.
#[test]
fn entries_over_one_variable_add_no_configuration_axis() {
    let fixture = conditional_requires_fixture();
    let mut constant: Option<Vec<u64>> = None;
    for n in [3u64, 6, 16, 32] {
        let sent = Sent::new(
            KEY_AND_A,
            conditioned_entries(n as usize, |_| "x".to_string(), |_| "a".to_string()),
            "c",
        );
        let (verdict, work) = sent_work(&fixture, &sent);
        verdict.unwrap();
        assert_eq!(work.entry_selections.get(), 2 * n + 3, "selections at {n}");
        assert_eq!(work.entry_literals.get(), n, "literals at {n}");
        assert_eq!(work.boolean_references.get(), n, "references at {n}");
        assert_eq!(work.projection_writes.get(), 6 * n + 9, "writes at {n}");
        assert_eq!(work.projection_scans.get(), 0, "scans at {n}");
        let same = vec![
            work.obligations.get(),
            work.largest_region_clauses.get(),
            work.projection_support_max.get(),
            work.held_contributions.get(),
            work.held_guard_literals.get(),
        ];
        assert_eq!(work.held_contributions.get(), 3, "held at {n}");
        match &constant {
            None => constant = Some(same),
            Some(constant) => assert_eq!(&same, constant, "the same at {n}"),
        }
    }
}

// Independent variables over one field, `n` of them, each entry reading `a` under its own `$xᵢ`:
// `a`'s regions are the `n` clauses `$x₁ ∨ … ∨ $xₙ`, and nothing multiplies them. Expected: each
// clause one availability and one rewrite-agreement obligation, so the obligations grow by exactly
// two per variable; the most clauses one region held is `n + 2`, the union of the regions the
// representation's top-level locations are present in, `__typename`'s, `id`'s and `a`'s `n`,
// before simplification absorbs it, never `2ⁿ`; the representation still holds three
// contributions.
//
// The work-control counterpart of the model's
// `independentEntryVariablesAddConfigurationAxesIsAccepted`, whose 2, 4 and 8 assignments for one,
// two and three variables this checker does not enumerate.
#[test]
fn independent_entry_variables_are_decided_apart() {
    let fixture = conditional_requires_fixture();
    let mut base: Option<u64> = None;
    for n in [1u64, 2, 3, 8] {
        let sent = Sent::new(
            KEY_AND_A,
            conditioned_entries(n as usize, |i| format!("x{i}"), |_| "a".to_string()),
            "c",
        );
        let (verdict, work) = sent_work(&fixture, &sent);
        verdict.unwrap();
        assert_eq!(work.entry_literals.get(), n, "literals at {n}");
        assert_eq!(work.held_contributions.get(), 3, "held at {n}");
        let (obligations, largest) = (work.obligations.get(), work.largest_region_clauses.get());
        let rest = *base.get_or_insert(obligations - 2 * n);
        assert_eq!(obligations, rest + 2 * n, "obligations at {n}");
        assert_eq!(largest, n + 2, "the most clauses at {n}");
    }
}

// `a` is never fetched and the entries read it under `$x` and under `$y`: the rejection names the
// read in a configuration of the entries' own variables, which the boundary read beforehand.
//
// The work-control counterpart of the model's `rejectedReadKeepsEntryConfigurationAxesIsRejected`.
#[test]
fn a_rejected_read_keeps_its_entry_variables() {
    let fixture = conditional_requires_fixture();
    let sent = Sent::new(
        KEY_FIELDS,
        conditioned_entries(2, |i| ["x", "y"][i - 1].to_string(), |_| "a".to_string()),
        "c",
    );
    let (verdict, work) = sent_work(&fixture, &sent);
    let error = verdict.unwrap_err().to_string();
    assert!(
        error.contains("the representation reads `a` at `t`, a `T` when $")
            && error.contains(READ_UNFETCHED),
        "{error}"
    );
    assert_eq!(work.boolean_references.get(), 2);
}

// An entry under `@include(if: false)` and `$x`, nesting one under `$y`: the boundary reads its
// three conditions, the literal and both variables, before anything is folded; the projection
// reads the outer two, excludes the entry at the literal and reads nothing below it, so no
// obligation keeps `$x` or `$y`: the obligations and the most variables one kept are those of the
// same plan without the entry.
//
// The work-control counterpart of the model's
// `nestedExcludedLiteralsKeepBothConfigurationAxesIsAccepted`.
#[test]
fn variables_behind_a_false_literal_are_read_at_the_boundary_alone() {
    let fixture = conditional_requires_fixture();
    let excluded = on_t(
        vec![include_literal(false), include("x")],
        vec![on_t(vec![include("y")], vec![input("a", vec![])])],
    );
    let with = Sent::new(KEY_FIELDS, vec![on_t(vec![], key_inputs()), excluded], "c");
    let (verdict, work) = sent_work(&fixture, &with);
    verdict.unwrap();
    assert_eq!(work.boolean_references.get(), 3);
    assert_eq!(work.entry_literals.get(), 2);
    let without = Sent::new(KEY_FIELDS, vec![on_t(vec![], key_inputs())], "c");
    let operation = declaring_booleans(["x", "y"], "{ t { c } }");
    let (verdict, alone) = family_check(&fixture, &without.plan(&fixture), &operation);
    verdict.unwrap();
    assert_eq!(work.obligations.get(), alone.obligations.get());
    assert_eq!(
        work.projection_support_max.get(),
        alone.projection_support_max.get()
    );
}

// Independent variables on separate obligations, `n` of them, each entry reading its own `fᵢ`
// under its own `$xᵢ`. Expected, before the run: the projection reads `2n + 3` selections and `n`
// literals, the boundary `n` references; it writes `5n + 10` regions, no entry's location written
// by another; the representation holds `n + 2` contributions; each `fᵢ` owes one availability and
// one rewrite-agreement obligation over `$xᵢ` alone, so the obligations grow by exactly two per
// entry; the most variables one obligation kept is the same at every `n`, and the most clauses one
// region held is `n + 2`, the union of the regions the top-level locations are present in before
// simplification absorbs it, where a product of the `n` variables would make either `2ⁿ`.
#[test]
fn independent_variables_on_separate_obligations_grow_by_entry() {
    let mut base: Option<(u64, u64)> = None;
    for n in [4u64, 8, 16, 32] {
        let fixture = conditional_world_with_fields(n as usize);
        let fetched = format!(
            "__typename id {}",
            (1..=n)
                .map(|i| format!("f{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let sent = Sent::new(
            &fetched,
            conditioned_entries(n as usize, |i| format!("x{i}"), |i| format!("f{i}")),
            "c",
        );
        let (verdict, work) = sent_work(&fixture, &sent);
        verdict.unwrap();
        assert_eq!(work.entry_selections.get(), 2 * n + 3, "selections at {n}");
        assert_eq!(work.entry_literals.get(), n, "literals at {n}");
        assert_eq!(work.boolean_references.get(), n, "references at {n}");
        assert_eq!(work.projection_writes.get(), 5 * n + 10, "writes at {n}");
        assert_eq!(work.held_contributions.get(), n + 2, "held at {n}");
        assert_eq!(
            work.largest_region_clauses.get(),
            n + 2,
            "the most clauses at {n}"
        );
        let (rest, support) = *base.get_or_insert((
            work.obligations.get() - 2 * n,
            work.projection_support_max.get(),
        ));
        assert_eq!(work.obligations.get(), rest + 2 * n, "obligations at {n}");
        assert_eq!(work.projection_support_max.get(), support, "support at {n}");
    }
}

// What the key choice, a bare copy and the rewrite agreement cost, each stated before the run. A
// key covering every configuration is chosen outright: the planner's plan tries its one key and
// collects no failure. Where no key does, each key's failures are collected where every key before
// it failed: `V`'s `id` fails where `$x` is false, one clause, and `id2`, asked there alone,
// covers it and fails nowhere. A
// bare `u` copies the two readable locations under it, `right` and `extra`. A `typenameEquals`
// filter splits a location's agreement in two, so a renamer after it is asked of both halves: with
// the filtered renamer before an unrelated one, the agreement follows one state more at the one
// location the filter reaches, read once as an explicit selection and once as a demanded field,
// than with the same renamer unfiltered.
#[test]
fn the_key_choice_and_the_agreement_read_what_they_split() {
    let world = conditional_requires_fixture();
    let planner = Sent::new(
        "__typename id ... on T @include(if: $test) { a }",
        vec![
            on_t(vec![], key_inputs()),
            on_t(
                vec![include("test")],
                vec![input("__typename", vec![]), input("a", vec![])],
            ),
        ],
        "c b @include(if: $test)",
    );
    let (verdict, work) = sent_work(&world, &planner);
    verdict.unwrap();
    assert_eq!(work.key_candidates.get(), 1);
    assert_eq!(work.key_failure_clauses.get(), 0);
    let keys = Sent::new(
        "__typename id id2",
        vec![
            entry(
                Some("V"),
                vec![include("x")],
                vec![input("__typename", vec![]), input("id", vec![])],
            ),
            entry(
                Some("V"),
                vec![skip("x")],
                vec![input("__typename", vec![]), input("id2", vec![])],
            ),
        ],
        "c",
    )
    .parent("v")
    .cases("... on V { c }");
    let (verdict, work) = sent_work(&world, &keys);
    verdict.unwrap();
    assert_eq!(work.key_candidates.get(), 2);
    assert_eq!(work.key_failure_clauses.get(), 1);
    let copied = Sent::new(
        "__typename id u { right extra: left }",
        copied_u(),
        "onRight",
    );
    let (verdict, work) = sent_work(&world, &copied);
    verdict.unwrap();
    assert_eq!(work.copied_locations.get(), 2);

    let books = fixture();
    let renamed = |first: Vec<serde_json::Value>| {
        let plan = book_requires_plan(
            &books,
            "__typename id __require_0_isbn: isbn",
            aliased_isbn(),
            serde_json::json!([
                renamer(serde_json::Value::Array(first), "isbn"),
                renamer(serde_json::json!([key("foo")]), "bar")
            ]),
        );
        let (verdict, work) = family_check(&books, &plan, "{ feed { ... on Book { blurb } } }");
        verdict.unwrap();
        work.agreement_states.get()
    };
    let filtered = renamed(vec![typename_equals("Book"), key("__require_0_isbn")]);
    let unfiltered = renamed(vec![key("__require_0_isbn")]);
    assert_eq!(filtered, unfiltered + 2);
}

//--------------------------------------------------------------------------------------------------
// The repair round: ancestor names per demand, a path's last element, explanations per
// configuration, and keys failing below the copy the fetch runs on

/// A `Photo`'s `a` and a `Video`'s `b` fetched under `p`, a `Video`'s `x` under `r`, all copied
/// whole by a bare `media` and named back, in order, `media/[Photo]/p → a`, `media/[Video]/p → b`
/// and, when `final_rename`, `media/b/r → x`.
fn renamed_parts(final_rename: bool) -> Sent<'static> {
    let mut rewrites = vec![
        renamer(
            serde_json::json!([key("media"), typename_equals("Photo"), key("p")]),
            "a",
        ),
        renamer(
            serde_json::json!([key("media"), typename_equals("Video"), key("p")]),
            "b",
        ),
    ];
    if final_rename {
        rewrites.push(renamer(
            serde_json::json!([key("media"), key("b"), key("r")]),
            "x",
        ));
    }
    Sent::new(
        "__typename id media { __typename ... on Photo { p: a { x } } ... on Video { p: b { r: x } } }",
        vec![on_t(vec![], key_inputs_and(vec![input("media", vec![])]))],
        "forParts",
    )
    .rewrites(serde_json::Value::Array(rewrites))
}

// `forParts` demands a `Photo`'s `a { x }` and a `Video`'s `b { x }`, both read under `p`, the
// `Video`'s `x` under `r`. The model's agreement reads each demanded field under the names of the
// fields leading to it in its own configuration, `media.a.x` for a `Photo` and `media.b.x` for a
// `Video`, so the three renamers carry each read key to its name and back: the model accepts.
//
// A control of the demand's ancestor names, which a shared read key must keep per configuration.
#[test]
fn a_shared_read_key_keeps_each_configurations_ancestor_names() {
    renamed_parts(true).check().unwrap();
}

// Without `media/b/r → x` a `Video`'s `x` is sent as `r`, and the demanded `x` is not held: the
// model rejects.
//
// Its neighbour.
#[test]
fn a_shared_read_key_without_its_last_rename_is_rejected() {
    let error = renamed_parts(false).rejection();
    assert!(
        error.contains("does not hold `x` at `t.media.p`"),
        "{error}"
    );
}

// A renamer whose path ends at an index after `id` renames nothing, as `rename_key_at` follows it:
// the representation sends `id` as it is, and the model accepts.
//
// A control of a renamer's last element: only a path ending at a key renames.
#[test]
fn a_renamer_ending_at_an_index_renames_nothing() {
    Sent::new(KEY_FIELDS, vec![on_t(vec![], key_inputs())], "c")
        .rewrites(renaming(
            &[key("id"), serde_json::json!({ "AnyIndex": null })],
            "other",
        ))
        .check()
        .unwrap();
}

// Its neighbour ending at `id` sends the key away: the model rejects.
#[test]
fn a_renamer_ending_at_the_key_sends_it_away() {
    let error = Sent::new(KEY_FIELDS, vec![on_t(vec![], key_inputs())], "c")
        .rewrites(renaming(&[key("id")], "other"))
        .rejection();
    assert!(
        error.contains("the representation sends `id` at `t`, a `T` as `id`"),
        "{error}"
    );
}

// `V`'s `id` is sent where `$x` and `$y` are both false and `id2` where `$x` holds or `$y` does not:
// neither key is sent where `$x` is false and `$y` holds, and the model rejects there. Each key's
// failure is explained in that configuration, `id`'s too, which also fails where `$x` holds.
//
// A control of the explanation of a configuration no key covers.
#[test]
fn every_keys_failure_is_explained_where_no_key_covers() {
    let error = Sent::new(
        "__typename id id2",
        vec![
            entry(Some("V"), vec![], vec![input("__typename", vec![])]),
            entry(
                Some("V"),
                vec![skip("x"), skip("y")],
                vec![input("id", vec![])],
            ),
            entry(Some("V"), vec![include("x")], vec![input("id2", vec![])]),
            entry(Some("V"), vec![skip("y")], vec![input("id2", vec![])]),
        ],
        "c",
    )
    .parent("v")
    .cases("... on V { c }")
    .rejection();
    assert!(
        error.contains("through none of its resolvable keys when `v` is `V` ∧ ¬$x ∧ $y"),
        "{error}"
    );
    assert!(
        error.contains("does not hold `id` at `v`, a `V` when ¬$x ∧ $y")
            && error.contains("does not hold `id2` at `v`, a `V` when ¬$x ∧ $y"),
        "{error}"
    );
    assert!(!error.contains("a `V` when $x"), "{error}");
}

/// `W`'s `org`, `team` and `id2` fetched, `W` sent `entries` and asked for `c`.
fn w_sent(entries: Vec<serde_json::Value>) -> Sent<'static> {
    Sent::new("__typename org { id } team { id } id2", entries, "c")
        .parent("w")
        .cases("... on W { c }")
}

/// `container` sent in every configuration, its `id` under `condition`.
fn retained(container: &str, condition: serde_json::Value) -> serde_json::Value {
    input(
        container,
        vec![entry(None, vec![condition], vec![input("id", vec![])])],
    )
}

// `org` is sent in every configuration and its `id` only where `$x` holds, `id2` where it does not:
// the model resolves `org { id }` where `$x` holds and `id2` where it does not, and accepts. The
// key `org { id }` fails below `w`, at `w.org`, where `$x` is false.
//
// A control of a key failing below the copy the fetch runs on.
#[test]
fn a_composite_key_failing_below_the_fetch_is_covered_by_another_key() {
    w_sent(vec![
        entry(
            Some("W"),
            vec![],
            vec![input("__typename", vec![]), retained("org", include("x"))],
        ),
        entry(Some("W"), vec![skip("x")], vec![input("id2", vec![])]),
    ])
    .check()
    .unwrap();
}

// `org` and `team` are both sent in every configuration, `org.id` where `$x` holds and `team.id`
// where it does not: the model resolves `org { id }` and `team { id }` in turn, and accepts.
//
// A control of two composite keys failing on different branches.
#[test]
fn two_composite_keys_failing_on_different_branches_are_accepted() {
    w_sent(vec![entry(
        Some("W"),
        vec![],
        vec![
            input("__typename", vec![]),
            retained("org", include("x")),
            retained("team", skip("x")),
        ],
    )])
    .check()
    .unwrap();
}

// `team.id` under `$y` instead: where `$x` and `$y` are both false neither composite key is sent
// and `id2` is not either, so the model rejects there, the two composite keys failing on their own
// branches together, each explained there.
//
// Its neighbour.
#[test]
fn two_composite_keys_failing_together_on_different_branches_are_rejected() {
    let error = w_sent(vec![entry(
        Some("W"),
        vec![],
        vec![
            input("__typename", vec![]),
            retained("org", include("x")),
            retained("team", include("y")),
        ],
    )])
    .rejection();
    assert!(
        error.contains("through none of its resolvable keys")
            && error.contains("¬$x")
            && error.contains("¬$y"),
        "{error}"
    );
    assert!(
        error.contains("does not hold `id` at `w.org`")
            && error.contains("does not hold `id` at `w.team`")
            && error.contains("does not hold `id2` at `w`"),
        "{error}"
    );
}
