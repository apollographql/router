//! @requires shapes where the incremental planner used to fetch more than
//! the legacy planner. Each test plans the same operation with both planners
//! and fails, with both plans printed, when the incremental planner uses
//! more fetches. Incremental plans also go through the correctness checker
//! unless a test opts out with `validate_correctness = false`.

use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::query_plan::query_planner::IncrementalPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

use crate::query_plan::build_query_plan_support::BulbTwinPlanner;
use crate::query_plan::build_query_plan_support::shared_test_planner_with_bulb_twin;

fn twin_config() -> QueryPlannerConfig {
    QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            enabled: false,
            beam_width: 4,
            fuel: 100_000,
            ..Default::default()
        },
        ..Default::default()
    }
}

macro_rules! twin_planner {
    ( $( $subgraph_name: tt: $subgraph_schema: expr),+ $(,)? ) => {{
        $crate::query_plan::build_query_plan_support::test_planner_with_bulb_twin(
            insta::_function_name!(),
            twin_config(),
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
fn no_extra_fetches(
    planner: &BulbTwinPlanner,
    operation: &str,
    validate_correctness: bool,
) -> (QueryPlan, QueryPlan) {
    let document = parse_operation(planner, operation);
    let legacy = planner
        .build_query_plan(&document, None, Default::default())
        .expect("legacy plan");
    let bulb = plan_incremental(planner, &document, validate_correctness);
    let legacy_fetches = count_fetches(&legacy);
    let bulb_fetches = count_fetches(&bulb);
    assert!(
        bulb_fetches <= legacy_fetches,
        "incremental planner used {bulb_fetches} fetches, legacy used {legacy_fetches}\n\
         --- legacy ---\n{legacy}\n--- incremental ---\n{bulb}",
    );
    (legacy, bulb)
}

#[track_caller]
fn parse_operation(
    planner: &BulbTwinPlanner,
    operation: &str,
) -> apollo_compiler::validation::Valid<apollo_compiler::ExecutableDocument> {
    apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        operation,
        "operation.graphql",
    )
    .expect("valid graphql document")
}

#[track_caller]
fn plan_incremental(
    planner: &BulbTwinPlanner,
    document: &apollo_compiler::validation::Valid<apollo_compiler::ExecutableDocument>,
    validate_correctness: bool,
) -> QueryPlan {
    let bulb = planner.bulb();
    let plan = bulb
        .build_query_plan(document, None, Default::default())
        .expect("incremental plan");
    if validate_correctness {
        apollo_federation::correctness::check_plan(
            bulb.api_schema(),
            bulb.supergraph_schema(),
            bulb.subgraph_schemas(),
            document,
            &plan,
        )
        .unwrap_or_else(|err| panic!("incremental plan failed check_plan: {err}\n{plan}"));
    }
    plan
}

/// [`no_extra_fetches`], taking `validate_correctness = false` the way
/// `assert_plan!` does.
macro_rules! assert_no_extra_fetches {
    (validate_correctness = $validate: expr, $planner: expr, $operation: expr $(,)?) => {
        no_extra_fetches($planner, $operation, $validate)
    };
    ($planner: expr, $operation: expr $(,)?) => {
        no_extra_fetches($planner, $operation, true)
    };
}

/// The incremental plan alone, taking `validate_correctness = false` the
/// way `assert_plan!` does.
macro_rules! plan_bulb {
    (validate_correctness = $validate: expr, $planner: expr, $operation: expr $(,)?) => {
        plan_incremental($planner, &parse_operation($planner, $operation), $validate)
    };
}

/// S1 owns `T.listing`, S2 owns the listing's fields, and S3 computes
/// fields of `T` from listing fields. Most tests here plan against this one
/// supergraph, so it carries every field they need between them.
const LISTING_S1: &str = r#"
    type Query { t: T }
    type T @key(fields: "id") { id: ID! listing: Listing other: Listing }
    type Listing @key(fields: "id") { id: ID! }
"#;
const LISTING_S2: &str = r#"
    type Listing @key(fields: "id") {
        id: ID!
        name: String
        sku: String
        dims: String
        cost(currency: String!): Int
    }
"#;
const LISTING_S3: &str = r#"
    type T @key(fields: "id") {
        id: ID!
        listing: Listing @external
        price: Int @requires(fields: "listing { sku }")
        weight: Int @requires(fields: "listing { dims }")
        usd: Int @requires(fields: "listing { cost(currency: \"USD\") }")
        eur: Int @requires(fields: "listing { cost(currency: \"EUR\") }")
        tax: Int @requires(fields: "listing { cost(currency: \"USD\") }")
    }
    type Listing @key(fields: "id") {
        id: ID!
        sku: String @external
        dims: String @external
        cost(currency: String!): Int @external
    }
"#;

#[track_caller]
fn listing_planner() -> BulbTwinPlanner {
    shared_test_planner_with_bulb_twin(
        "requires_listing",
        twin_config(),
        &[("S1", LISTING_S1), ("S2", LISTING_S2), ("S3", LISTING_S3)],
    )
}

/// Two @requires on sibling fields walk the same entity for different
/// leaves. Neither condition field collides with another selection, so
/// both stay unaliased and share one listing fetch.
#[test]
fn req_sibling_conditions_on_same_entity_share_fetch() {
    assert_no_extra_fetches!(&listing_planner(), "{ t { price weight } }");
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
    assert_no_extra_fetches!(&planner, "{ t { price weight } }");
}

/// The parent path is gated by @include. A gate does not change which field
/// a response key holds, so the condition still shares the client's
/// `listing` and the listing entity is fetched once.
#[test]
fn req_client_field_under_include_shares_fetch() {
    assert_no_extra_fetches!(
        &listing_planner(),
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
    let (_, bulb) = assert_no_extra_fetches!(&planner, "{ t { a c } }");
    let rendered = bulb.to_string();
    assert!(
        !rendered.contains("__require_"),
        "no condition field here can collide with a client field, so none needs an alias:\n{rendered}",
    );
}

/// When the client selects the walked field itself, the condition rides
/// the client's fetch chain and nothing is duplicated.
#[test]
fn req_client_field_lets_condition_ride_its_fetch() {
    assert_no_extra_fetches!(&listing_planner(), "{ t { price listing { name } } }");
}

/// With the client's copy present, both consumers' conditions merge into
/// it as well.
#[test]
fn req_two_consumers_with_client_field_share_fetch() {
    assert_no_extra_fetches!(
        &listing_planner(),
        "{ t { price weight listing { name } } }",
    );
}

/// The client puts a different field under the condition field's response
/// key, so the condition cannot share it and must be aliased.
#[test]
fn req_client_alias_on_condition_key_forces_alias() {
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    let plan = plan_bulb!(
        validate_correctness = false,
        &listing_planner(),
        "{ t { listing: other { name } price } }",
    );
    let rendered = plan.to_string();
    assert!(
        rendered.contains("__require_0_listing: listing"),
        "the client's `listing` key holds `other`, so the condition needs an alias:\n{rendered}",
    );
}

/// The condition is resolvable in place through `@provides`, and the client
/// puts a different field under its response key. The in-place condition
/// needs an alias just like a key-hopped one.
#[test]
fn req_client_alias_on_in_place_condition_key_forces_alias() {
    let planner = twin_planner!(
        S1: r#"
            type T @key(fields: "id") {
                id: ID!
                listing: Listing @shareable
                other: Listing @shareable
            }
            type Listing @key(fields: "id") {
                id: ID!
                sku: String @shareable
                name: String @shareable
            }
        "#,
        S3: r#"
            type Query { t: T @provides(fields: "listing { sku }") }
            type T @key(fields: "id") {
                id: ID!
                listing: Listing @external
                other: Listing @shareable
                price: Int @requires(fields: "listing { sku }")
            }
            type Listing @key(fields: "id") {
                id: ID!
                sku: String @external
            }
        "#,
    );
    // The legacy planner fetches `listing { sku name }` in place of
    // `other { name }` here, so only the incremental plan is checked.
    let plan = plan_bulb!(
        validate_correctness = true,
        &planner,
        "{ t { listing: other { name } price } }",
    );
    let rendered = plan.to_string();
    assert!(
        rendered.contains("__require_0_listing: listing"),
        "the client's `listing` key holds `other`, so the condition needs an alias:\n{rendered}",
    );
}

/// Two conditions walk the same field but disagree on a nested field's
/// arguments. Sharing the top-level key would put both `cost` selections
/// under one response key, so each condition needs its own alias.
#[test]
fn req_nested_argument_conflict_between_conditions_forces_aliases() {
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    let plan = plan_bulb!(
        validate_correctness = false,
        &listing_planner(),
        "{ t { usd eur } }",
    );
    let rendered = plan.to_string();
    assert!(
        rendered.contains("__require_0_listing: listing")
            && rendered.contains("__require_1_listing: listing"),
        "conflicting nested arguments need two aliases:\n{rendered}",
    );
}

/// Two consumers need the same aliased condition. They share the alias, so
/// the condition is fetched once and both consumers read it from one
/// entity fetch.
#[test]
fn req_identical_aliased_conditions_share_one_fetch() {
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    let (_, bulb) = assert_no_extra_fetches!(
        validate_correctness = false,
        &listing_planner(),
        "{ t { usd tax } }",
    );
    let rendered = bulb.to_string();
    assert!(
        rendered.contains("__require_0_listing: listing") && !rendered.contains("__require_1_"),
        "both conditions should share one alias:\n{rendered}",
    );
}
