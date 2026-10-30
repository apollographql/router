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
