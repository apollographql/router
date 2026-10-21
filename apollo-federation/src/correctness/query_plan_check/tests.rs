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
use apollo_compiler::validation::Valid;

use super::super::CheckerOptions;
use super::super::CorrectnessError;
use crate::Supergraph;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;

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
            PlanNode::Condition(_) => unimplemented!("not needed by the written plans"),
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
            TopLevelPlanNode::Subscription(_) | TopLevelPlanNode::Condition(_) => {
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
            PlanNode::Condition(_) => unreachable!("only the variants matched above"),
        };
        QueryPlan {
            node: Some(node),
            statistics: Default::default(),
        }
    }
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

// One entity fetch over two types whose `@key` field sets name different fields, so the requires
// entries and the entity cases are not interchangeable.
//
// This does *not* reach the branch that reads one type's `@key` at another's entry: the table is
// filled from the pairs of equal type outwards, and on a plan the planner produced those settle
// every row and column, so no other pair is ever computed. Reaching it needs a plan whose diagonal
// fails, which the planner does not emit -- see the note in `mod.rs` on what still has no test.
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

// The interface field alongside the entity cases, so the fetch carries a selection that is not
// under any type condition as well as the two that are.
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
        rejection(plan_json!("locations_id"), "{ locations { id name } }"),
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
        rejection(
            plan_json!("locations_id"),
            "{ locations { reviews { rating } } }",
        ),
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
    let fixture = fixture();
    let plan = aliased_requires_plan(&fixture, "__typename id __require_0_isbn: id", "id");
    let error = check_blurb_plan(&fixture, &plan).expect_err("`isbn` would carry `id`'s value");
    assert!(
        error
            .to_string()
            .contains("does not agree with the fetch's input rewrites"),
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
        error
            .to_string()
            .contains("has not fetched what the subgraph demands"),
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

// The subgraph oracle answers in the subgraph's own terms, and `A` declares `I` as an interface
// object — an *object* there, naming itself and nothing else. Ungrounded, that name intersects
// the supergraph's `{P, Q}` to nothing, so every obligation under `start` was dropped and a plan
// that fetches nothing for `data` was accepted. Both checkers share the oracle, so both were
// blind; `SubgraphConstraint` grounds the name back now.
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
// `Owner`, which states the path's condition: it is mounted as one copy under that fragment,
// unnarrowed past the store, the last level its path shares with the fetch for `price`, so every
// product is sent its owner's `locale`.
//
// A control of changed behaviour: guarding the field again by both names mounts a copy narrowed to
// each owner type, neither serving a fetch that runs whatever the owner is, and the plan is
// rejected though the value was fetched for every owner.
#[test]
fn a_context_value_under_a_fragment_stating_its_condition_is_accepted() {
    let fixture = owner_profile_fixture();
    let plan = owner_profile_plan(&fixture, &["Person", "Company"]);
    fixture.check(&plan, STORES_PRODUCTS_PRICE).unwrap();
}

// The `locale` fetched only where the owner is a `Person`: a company's products are sent none.
//
// A control of preserved behaviour: the fragment on `Owner` reaches `Company`, which the path
// excludes, so it states nothing and the field keeps its guard on `Person`.
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
// A control of changed behaviour: below a key with more path the field `author` was copied under a
// fragment on `Item`, which inclusion grounds to every item, and the end of a path did not read its
// condition at all.
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
// A control of preserved behaviour at the inclusion boundary: the mounting keeps the author's
// `name` under a fragment on `Person` inside `featured`, which no item satisfies, and inclusion
// credits it with nothing.
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
