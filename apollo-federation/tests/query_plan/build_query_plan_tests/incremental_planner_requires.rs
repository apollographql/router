//! @requires shapes where the incremental planner's plans diverge from the
//! legacy planner's. Each test plans the same operation with both planners
//! and fails, with both plans printed, when the incremental planner uses
//! more fetches. The tests that pass today document the one case where
//! condition data already dedupes with the client's selection, so the
//! rework keeps it.

use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::query_plan::query_planner::IncrementalPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

use crate::query_plan::build_query_plan_support::BulbTwinPlanner;

macro_rules! twin_planner {
    ( $( $subgraph_name: tt: $subgraph_schema: expr),+ $(,)? ) => {{
        $crate::query_plan::build_query_plan_support::test_planner_with_bulb_twin(
            insta::_function_name!(),
            QueryPlannerConfig {
                incremental_planner: IncrementalPlannerConfig {
                    enabled: false,
                    beam_width: 4,
                    fuel: 100_000,
                    ..Default::default()
                },
                ..Default::default()
            },
            &[ $( (subgraph_name!($subgraph_name), $subgraph_schema) ),+ ],
        )
    }};
}

fn count_fetches_in(node: &PlanNode) -> usize {
    match node {
        PlanNode::Fetch(_) => 1,
        PlanNode::Sequence(seq) => seq.nodes.iter().map(count_fetches_in).sum(),
        PlanNode::Parallel(par) => par.nodes.iter().map(count_fetches_in).sum(),
        PlanNode::Flatten(flatten) => count_fetches_in(&flatten.node),
        PlanNode::Condition(cond) => {
            cond.if_clause.as_deref().map_or(0, count_fetches_in)
                + cond.else_clause.as_deref().map_or(0, count_fetches_in)
        }
        PlanNode::Defer(_) => panic!("defer is not used in these tests"),
    }
}

fn count_fetches(plan: &QueryPlan) -> usize {
    match &plan.node {
        None => 0,
        Some(TopLevelPlanNode::Fetch(_)) => 1,
        Some(TopLevelPlanNode::Sequence(seq)) => seq.nodes.iter().map(count_fetches_in).sum(),
        Some(TopLevelPlanNode::Parallel(par)) => par.nodes.iter().map(count_fetches_in).sum(),
        Some(TopLevelPlanNode::Flatten(flatten)) => count_fetches_in(&flatten.node),
        Some(TopLevelPlanNode::Condition(cond)) => {
            cond.if_clause.as_deref().map_or(0, count_fetches_in)
                + cond.else_clause.as_deref().map_or(0, count_fetches_in)
        }
        Some(other) => panic!("unexpected top-level node: {other:?}"),
    }
}

/// Plan `operation` with both planners and fail if the incremental planner
/// uses more fetches than the legacy one.
#[track_caller]
fn assert_no_extra_fetches(planner: &BulbTwinPlanner, operation: &str) -> (QueryPlan, QueryPlan) {
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        operation,
        "operation.graphql",
    )
    .expect("valid graphql document");
    let legacy = planner
        .build_query_plan(&document, None, Default::default())
        .expect("legacy plan");
    let bulb = planner
        .bulb()
        .build_query_plan(&document, None, Default::default())
        .expect("incremental plan");
    let legacy_fetches = count_fetches(&legacy);
    let bulb_fetches = count_fetches(&bulb);
    assert!(
        bulb_fetches <= legacy_fetches,
        "incremental planner used {bulb_fetches} fetches, legacy used {legacy_fetches}\n\
         --- legacy ---\n{legacy}\n--- incremental ---\n{bulb}",
    );
    (legacy, bulb)
}

/// S1 owns `T.listing`, S2 owns the listing's fields, and S3 computes
/// `T.price` and `T.weight` from different listing fields.
const LISTING_S1: &str = r#"
    type Query { t: T }
    type T @key(fields: "id") { id: ID! listing: Listing }
    type Listing @key(fields: "id") { id: ID! }
"#;
const LISTING_S2: &str = r#"
    type Listing @key(fields: "id") { id: ID! name: String sku: String dims: String }
"#;
const LISTING_S3: &str = r#"
    type T @key(fields: "id") {
        id: ID!
        listing: Listing @external
        price: Int @requires(fields: "listing { sku }")
        weight: Int @requires(fields: "listing { dims }")
    }
    type Listing @key(fields: "id") { id: ID! sku: String @external dims: String @external }
"#;

/// Two @requires on sibling fields walk the same entity for different
/// leaves. Neither condition contains the other, so each gets its own
/// alias and its own entity fetch chain, where one listing fetch would do.
#[test]
fn req_sibling_conditions_on_same_entity_share_fetch() {
    let planner = twin_planner!(S1: LISTING_S1, S2: LISTING_S2, S3: LISTING_S3);
    assert_no_extra_fetches(&planner, "{ t { price weight } }");
}

/// Same as above with the consumers in different subgraphs: the listing
/// chain is still shared data and should be fetched once.
#[test]
fn req_consumers_in_different_subgraphs_share_fetch() {
    let planner = twin_planner!(
        S1: LISTING_S1,
        S2: LISTING_S2,
        S3: r#"
            type T @key(fields: "id") {
                id: ID!
                listing: Listing @external
                price: Int @requires(fields: "listing { sku }")
            }
            type Listing @key(fields: "id") { id: ID! sku: String @external }
        "#,
        S4: r#"
            type T @key(fields: "id") {
                id: ID!
                listing: Listing @external
                weight: Int @requires(fields: "listing { dims }")
            }
            type Listing @key(fields: "id") { id: ID! dims: String @external }
        "#,
    );
    assert_no_extra_fetches(&planner, "{ t { price weight } }");
}

/// The parent path is gated by @include. Sharing with the client's copy
/// of `listing` is disabled on conditional paths, so the condition is
/// staged under an alias and the listing entity is fetched twice.
#[test]
fn req_client_field_under_include_shares_fetch() {
    let planner = twin_planner!(S1: LISTING_S1, S2: LISTING_S2, S3: LISTING_S3);
    assert_no_extra_fetches(
        &planner,
        "query($v: Boolean!) { t @include(if: $v) { price listing { name } } }",
    );
}

/// Part of the condition is resolvable where the entity was fetched and
/// part needs a key hop. Nothing in the operation can collide with either
/// field, so neither needs an alias.
#[test]
fn req_partially_in_place_condition_needs_no_alias() {
    let planner = twin_planner!(
        S1: r#"
            type Query { t: T }
            type T @key(fields: "id") { id: ID! a: Int }
        "#,
        S2: r#"
            type T @key(fields: "id") { id: ID! b: Int }
        "#,
        S3: r#"
            type T @key(fields: "id") {
                id: ID!
                a: Int @external
                b: Int @external
                c: Int @requires(fields: "a b")
            }
        "#,
    );
    let (_, bulb) = assert_no_extra_fetches(&planner, "{ t { a c } }");
    let rendered = bulb.to_string();
    assert!(
        !rendered.contains("__require_"),
        "no condition field here can collide with a client field, so none needs an alias:\n{rendered}",
    );
}

/// When the client selects the walked field itself, the condition rides
/// the client's fetch chain and nothing is duplicated. This is the case
/// the rework must keep.
#[test]
fn req_client_field_lets_condition_ride_its_fetch() {
    let planner = twin_planner!(S1: LISTING_S1, S2: LISTING_S2, S3: LISTING_S3);
    assert_no_extra_fetches(&planner, "{ t { price listing { name } } }");
}

/// With the client's copy present, both consumers' conditions merge into
/// it as well.
#[test]
fn req_two_consumers_with_client_field_share_fetch() {
    let planner = twin_planner!(S1: LISTING_S1, S2: LISTING_S2, S3: LISTING_S3);
    assert_no_extra_fetches(&planner, "{ t { price weight listing { name } } }");
}
