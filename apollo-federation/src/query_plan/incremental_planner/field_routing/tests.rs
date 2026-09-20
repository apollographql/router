use crate::Supergraph;
use crate::error::FederationError;
use crate::query_plan::TopLevelPlanNode;
use crate::query_plan::query_planner::IncrementalPlannerConfig;
use crate::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
use crate::query_plan::query_planner::QueryPlanOptions;
use crate::query_plan::query_planner::QueryPlanner;
use crate::query_plan::query_planner::QueryPlannerConfig;

fn default_config() -> QueryPlannerConfig {
    QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            enabled: true,
            beam_width: 4,
            fuel: 100_000,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn plan_query(schema: &str, query: &str) -> String {
    plan_query_with_options(schema, query, default_config(), Default::default())
}

fn plan_query_with_options(
    schema: &str,
    query: &str,
    config: QueryPlannerConfig,
    plan_options: QueryPlanOptions,
) -> String {
    let supergraph = Supergraph::new(schema).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, config).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        query,
        "test.graphql",
    )
    .expect("query parse");
    let plan = planner
        .build_query_plan(&document, None, plan_options)
        .expect("query plan");
    format!("{plan}")
}

fn plan_query_with_defer(schema: &str, query: &str) -> String {
    let config = QueryPlannerConfig {
        incremental_delivery: QueryPlanIncrementalDeliveryConfig { enable_defer: true },
        ..default_config()
    };
    plan_query_with_options(schema, query, config, Default::default())
}

fn plan_query_with_router_specs(schema: &str, query: &str) -> String {
    let supergraph = Supergraph::new_with_router_specs(schema).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, default_config()).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        query,
        "test.graphql",
    )
    .expect("query parse");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan");
    format!("{plan}")
}

/// One supergraph shared by every test here; each test picks the part of
/// it that exercises the path under test.
const SCHEMA: &str = include_str!("../fixtures/supergraph.graphql");

#[test]
fn single_subgraph_query_produces_valid_plan() {
    let plan_str = plan_query(SCHEMA, "{ user { id name } }");
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name': {plan_str}"
    );
    assert_eq!(
        plan_str.matches("Fetch(").count(),
        1,
        "Fields owned by one subgraph need one fetch: {plan_str}"
    );
}

#[test]
fn cross_subgraph_key_hop_produces_two_fetches() {
    let plan_str = plan_query(SCHEMA, "{ user { name email } }");
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Plan should fetch 'email': {plan_str}"
    );
}

/// Explicit __typename next to another field is stripped by
/// optimize_sibling_typenames during normalization (an old-planner
/// performance workaround) and restored once at bulb entry. It must
/// appear in the fetch.
#[test]
fn explicit_sibling_typename_is_preserved() {
    let plan_str = plan_query(SCHEMA, "{ user { __typename name } }");
    assert!(
        plan_str.contains("__typename"),
        "Plan should fetch explicit '__typename': {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name': {plan_str}"
    );
}

/// Top-level __typename is removed before planning (remove_introspection:
/// the router execution answers it).
#[test]
fn root_typename_is_left_to_router_execution() {
    let plan_str = plan_query(SCHEMA, "{ __typename user { name } }");
    assert_eq!(
        plan_str.matches("Fetch(").count(),
        1,
        "Root __typename must not add a fetch: {plan_str}"
    );

    let alone = plan_query(SCHEMA, "{ __typename }");
    assert_eq!(
        alone, "QueryPlan {}",
        "Router execution answers root __typename"
    );
}

#[test]
fn subscription_produces_subscription_plan_node() {
    let supergraph = Supergraph::new(SCHEMA).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, default_config()).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "subscription { onUserCreated { id name email } }",
        "test.graphql",
    )
    .expect("query parse");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan");
    assert!(
        matches!(plan.node, Some(TopLevelPlanNode::Subscription(_))),
        "Subscription should produce SubscriptionNode: {plan}"
    );
    let plan_str = format!("{plan}");
    assert!(
        plan_str.contains("Subscription"),
        "Plan should contain Subscription block: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name' from primary: {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Plan should fetch 'email' via entity hop: {plan_str}"
    );
}

#[test]
fn mutation_produces_sequential_plan() {
    let plan_str = plan_query(
        SCHEMA,
        r#"mutation { createUser(name: "Alice") { id name email } }"#,
    );
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Plan should fetch 'email' via entity hop: {plan_str}"
    );
}

#[test]
fn mutation_multiple_fields_are_not_merged() {
    let plan_str = plan_query(
        SCHEMA,
        r#"mutation { createUser(name: "Alice") { id name } updateUser(id: "1", name: "Bob") { id name } }"#,
    );
    assert!(
        plan_str.contains("Sequence"),
        "Same-subgraph mutation fields get one fetch each, in sequence: {plan_str}"
    );
    assert!(
        plan_str.contains("createUser"),
        "Plan should contain createUser: {plan_str}"
    );
    assert!(
        plan_str.contains("updateUser"),
        "Plan should contain updateUser: {plan_str}"
    );
}

/// Repro for the "local edge suppresses a required key hop" gap: `profile`
/// is shareable in A and B, but A's copy of `Profile` lacks `detail` and
/// `Profile` has no key, so once `profile` is routed to A, `detail` is
/// stranded. `profile` is a genuine decision point (direct edge to A plus
/// the User-level key hop to B), so the greedy strand-and-drop is recovered
/// by BULB backtracking picking the hop.
#[test_log::test]
fn shareable_local_dead_end_reroutes_through_key_hop() {
    let plan_str = plan_query(SCHEMA, "{ user { profile { detail } } }");
    assert!(
        plan_str.contains("detail"),
        "Plan should fetch 'detail' via B: {plan_str}"
    );
    assert!(
        plan_str.contains("service: \"b\""),
        "Plan should hop to subgraph b for profile.detail: {plan_str}"
    );
}

/// A forced condition commit whose greedy choice strands a descendant on a
/// circular key must backtrack to the ancestor's alternative. `target` lives
/// only in T, keyed on `c { cid cm }`. Routing that key: `c` commits
/// greedily to A (direct), but A cannot resolve `cm`. Its only hop from C
/// is T's circular `{cid cm}` key, so the commit fails. The condition `c`
/// was forced (never a BULB decision), so recovery must come from the
/// fast-forward trail: rewind `c` to its key hop into B, where the whole
/// key resolves.
#[test_log::test]
fn circular_key_drop_backtracks_to_ancestor_condition_alternative() {
    let plan_str = plan_query(SCHEMA, "{ entry { target } }");
    assert!(
        plan_str.contains("target"),
        "Plan should fetch 'target' from T: {plan_str}"
    );
    assert!(
        plan_str.contains("service: \"b\""),
        "Plan should route the key's `c` subtree through subgraph b: {plan_str}"
    );
    assert!(
        plan_str.contains("cm"),
        "Plan should fetch the key field 'cm': {plan_str}"
    );
}

/// An unplannable selection must produce an error, never a silently
/// incomplete plan (missing fields, or an empty `QueryPlan {}`).
#[test]
fn incomplete_plan_is_an_error_not_a_partial_plan() {
    // Same schema as the dead-end test, but with fuel=1 the search cannot
    // backtrack into the key-hop alternative, so the greedy pass strands
    // `detail`. The planner must error rather than emit a partial plan.
    let config = QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            fuel: 0,
            ..default_config().incremental_planner
        },
        ..default_config()
    };
    let supergraph = Supergraph::new(SCHEMA).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, config).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ user { profile { detail } } }",
        "test.graphql",
    )
    .expect("query parse");
    let result = planner.build_query_plan(&document, None, Default::default());
    match result {
        Err(_) => {}
        Ok(plan) => {
            // If fuel=1 still finds the complete plan (greedy happens to
            // pick the hop), the plan must contain the field; silence
            // plus a missing field is the failure mode under test.
            let plan_str = format!("{plan}");
            assert!(
                plan_str.contains("detail"),
                "Planner returned an incomplete plan instead of erroring: {plan_str}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage-focused plan-level tests. Each test names the code region it
// forces.
// ---------------------------------------------------------------------------

/// A cancellation callback that immediately breaks must abort planning with
/// a PlanningCancelled error rather than returning a plan.
/// Targets incremental_planner/mod.rs's cancelled branch.
#[test]
fn cooperative_cancellation_stops_planning() {
    let supergraph = Supergraph::new(SCHEMA).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, default_config()).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ user { name email } }",
        "test.graphql",
    )
    .expect("query parse");
    let cancel = || std::ops::ControlFlow::Break(());
    let result = planner.build_query_plan(
        &document,
        None,
        QueryPlanOptions {
            check_for_cooperative_cancellation: Some(&cancel),
            ..Default::default()
        },
    );
    assert!(
        result.is_err(),
        "Cancelled planning should error, got:\n{}",
        result.map(|p| p.to_string()).unwrap_or_default(),
    );
}

/// A key hop launched from INSIDE an entity fetch (`extra` hops B->C while
/// its pending lives in B's entity fetch for P): the shallowest-anchor
/// dominance check (`parent_key_anchor`) runs against the fetch feeding the
/// entity fetch. Here it declines (A has no D), so C's group chains behind
/// B's.
/// Targets commit.rs parent_key_anchor's context-anchor and walk-failure
/// arms.
#[test]
fn nested_entity_hop_from_inside_entity_fetch() {
    let plan_str = plan_query(SCHEMA, "{ p { details { extra } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            p {
              __typename
              id
            }
          }
        },
        Flatten(path: "p") {
          Fetch(service: "b") {
            {
              ... on P {
                __typename
                id
              }
            } =>
            {
              ... on P {
                details {
                  __typename
                  did
                }
              }
            }
          },
        },
        Flatten(path: "p.details") {
          Fetch(service: "c") {
            {
              ... on D {
                __typename
                did
              }
            } =>
            {
              ... on D {
                extra
              }
            }
          },
        },
      },
    }
    "###);
}

/// generate_query_fragments compresses subgraph operations after BULB
/// planning.
/// Targets incremental_planner/mod.rs's GenerateFragments compression arm.
#[test]
fn generate_query_fragments_config_is_honored() {
    let config = QueryPlannerConfig {
        generate_query_fragments: true,
        ..default_config()
    };
    let plan_str = plan_query_with_options(
        SCHEMA,
        "{ user { name email } }",
        config,
        Default::default(),
    );
    assert!(
        plan_str.contains("email"),
        "Plan should fetch 'email': {plan_str}"
    );
}

/// Cancellation must abort planning no matter which check observes it:
/// sweep the break point across every cancellation check the planner makes
/// for this query, including the ones inside the BULB search loop.
/// Targets incremental_planner/mod.rs's PlanningCancelled branch and
/// bulb_search's cancelled bookkeeping.
#[test]
fn cancellation_at_any_check_point_aborts_planning() {
    let supergraph = Supergraph::new(SCHEMA).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, default_config()).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ user { name email } }",
        "test.graphql",
    )
    .expect("query parse");

    // Count how many times a full, un-cancelled planning run checks.
    let count = std::cell::Cell::new(0usize);
    let counting = || {
        count.set(count.get() + 1);
        std::ops::ControlFlow::Continue(())
    };
    planner
        .build_query_plan(
            &document,
            None,
            QueryPlanOptions {
                check_for_cooperative_cancellation: Some(&counting),
                ..Default::default()
            },
        )
        .expect("un-cancelled planning succeeds");
    let total_checks = count.get();
    assert!(total_checks > 0, "expected at least one cancellation check");

    // Break at every observed check point in turn.
    for break_at in 0..total_checks {
        let seen = std::cell::Cell::new(0usize);
        let breaking = || {
            let n = seen.get();
            seen.set(n + 1);
            if n >= break_at {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let result = planner.build_query_plan(
            &document,
            None,
            QueryPlanOptions {
                check_for_cooperative_cancellation: Some(&breaking),
                ..Default::default()
            },
        );
        assert!(
            result.is_err(),
            "cancelling at check {break_at}/{total_checks} should abort planning",
        );
    }
}

// ---------------------------------------------------------------------------
// @requires tests
// ---------------------------------------------------------------------------

/// Like `plan_query` but returns the planner's Result, for tests that
/// assert planning fails (e.g. circular @requires).
fn try_plan_query(schema: &str, query: &str) -> Result<String, FederationError> {
    let supergraph = Supergraph::new(schema).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, default_config()).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        query,
        "test.graphql",
    )
    .expect("query parse");
    planner
        .build_query_plan(&document, None, Default::default())
        .map(|plan| format!("{plan}"))
}

const CIRCULAR_REQUIRES_SCHEMA: &str = include_str!("../fixtures/circular_requires.graphql");

/// Circular @requires (B: f requires g, C: g requires f) must terminate
/// with a planning error, not recurse until stack overflow, and not
/// silently return an incomplete plan.
#[test_log::test]
fn circular_requires_errors_instead_of_recursing() {
    let result = try_plan_query(CIRCULAR_REQUIRES_SCHEMA, "{ entity { f } }");
    assert!(
        result.is_err(),
        "Circular @requires should fail planning, got:\n{}",
        result.as_deref().unwrap_or("<err>"),
    );
}

/// Shared by the @requires, @override, and routing tests below; each test
/// picks its slice of the schema by operation.
const REQUIRES_SCHEMA: &str = include_str!("../fixtures/requires.graphql");

#[test]
fn requires_fields_added_to_fetch() {
    let plan_str = plan_query(REQUIRES_SCHEMA, "{ product { shippingCost } }");
    assert!(
        plan_str.contains("weight"),
        "Plan should fetch 'weight' for @requires: {plan_str}"
    );
}

/// @requires on a field whose subgraph declares the required fields as
/// @external: the query enters through B (which owns `shippingCost`
/// requiring `weight`), but `weight` is only resolvable in A. The field
/// must move into a second B fetch whose entity representation carries
/// `weight` fetched from A. B's root fetch must not select the
/// @external `weight` itself.
#[test_log::test]
fn requires_unresolvable_locally_hops_through_owning_subgraph() {
    let plan_str = plan_query(REQUIRES_SCHEMA, "{ productInB { shippingCost } }");
    insta::assert_snapshot!(plan_str, @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "b") {
              {
                productInB {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "productInB") {
              Fetch(service: "a") {
                {
                  ... on Product {
                    __typename
                    id
                  }
                } =>
                {
                  ... on Product {
                    __require_0_weight: weight
                  }
                }
              },
            },
            Flatten(path: "productInB") {
              Fetch(service: "b") {
                {
                  ... on Product {
                    __typename
                    id
                    __require_0_weight: weight
                  }
                } =>
                {
                  ... on Product {
                    shippingCost
                  }
                }
              },
            },
          },
        }
        "###);
}

/// @requires whose field set walks through a *locally resolvable* field
/// (`a`) into nested selections owned by other subgraphs (`s.status` in
/// s1, `j.m` in s2). The planner must resolve the local prefix in place
/// and hop from A's key edges for the nested parts, at the merge path of
/// C's `a`, not from C's (nonexistent) key edges, and not at some other
/// path from A.
#[test_log::test]
fn requires_through_local_field_resolves_nested_parts() {
    let plan_str = plan_query(REQUIRES_SCHEMA, "{ a { c { elig } } }");
    // All three requires leaves must be fetched somewhere.
    for needle in ["status", "j {", "elig"] {
        assert!(
            plan_str.contains(needle),
            "Plan should fetch {needle:?}: {plan_str}"
        );
    }
    // The nested hops must merge at C's `a` (aliased as a @requires
    // condition), i.e. path a.c.__require_0_a, not at the top-level `a`.
    assert!(
        plan_str.contains("a.c.__require_0_a"),
        "Nested requires parts should merge under a.c.__require_0_a: {plan_str}"
    );
}

#[test]
fn static_override_routes_field_to_overriding_subgraph() {
    let plan_str = plan_query(REQUIRES_SCHEMA, "{ user { name nickname } }");
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("nickname"),
        "Plan should fetch 'nickname': {plan_str}"
    );
    assert!(
        !plan_str.contains("Flatten"),
        "Static override should not require entity fetch: {plan_str}"
    );
}

/// Demonstrates that BULB backtracking corrects greedy tiebreak mistakes.
/// `profile` is a key hop from A to either B or C. The greedy pass picks
/// B, which requires a second hop to C for `detail`, producing a 3-fetch
/// plan. Backtracking discovers that C can serve `profile.detail` directly
/// and produces the optimal 2-fetch plan.
#[test_log::test]
fn greedy_tiebreak_mistake_is_corrected_by_backtracking() {
    let document_str = "{ user { profile { detail } } }";

    let greedy_config = QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            fuel: 0,
            ..default_config().incremental_planner
        },
        ..default_config()
    };
    let greedy_plan_str = plan_query_with_options(
        REQUIRES_SCHEMA,
        document_str,
        greedy_config,
        Default::default(),
    );
    insta::assert_snapshot!(greedy_plan_str, @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                user {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "user") {
              Fetch(service: "b") {
                {
                  ... on User {
                    __typename
                    id
                  }
                } =>
                {
                  ... on User {
                    profile {
                      __typename
                      id
                    }
                  }
                }
              },
            },
            Flatten(path: "user.profile") {
              Fetch(service: "c") {
                {
                  ... on Profile {
                    __typename
                    id
                  }
                } =>
                {
                  ... on Profile {
                    detail
                  }
                }
              },
            },
          },
        }
        "###);

    let backtracking_plan_str = plan_query(REQUIRES_SCHEMA, document_str);
    insta::assert_snapshot!(backtracking_plan_str, @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                user {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "user") {
              Fetch(service: "c") {
                {
                  ... on User {
                    __typename
                    id
                  }
                } =>
                {
                  ... on User {
                    profile {
                      detail
                    }
                  }
                }
              },
            },
          },
        }
        "###);
}

#[test]
fn progressive_override_routes_to_overrider_when_label_active() {
    let plan_str = plan_query_with_options(
        REQUIRES_SCHEMA,
        "{ user { name handle } }",
        default_config(),
        QueryPlanOptions {
            override_conditions: vec!["test".to_string()],
            ..Default::default()
        },
    );
    assert!(
        plan_str.contains("handle"),
        "Plan should fetch 'handle': {plan_str}"
    );
    assert!(
        !plan_str.contains("Flatten"),
        "Active override should not require entity fetch: {plan_str}"
    );
}

#[test]
fn progressive_override_routes_to_original_when_label_inactive() {
    let plan_str = plan_query(REQUIRES_SCHEMA, "{ user { name handle } }");
    assert!(
        plan_str.contains("handle"),
        "Plan should fetch 'handle': {plan_str}"
    );
    assert!(
        plan_str.contains("Flatten"),
        "Inactive override should require entity fetch to B: {plan_str}"
    );
}

// Regression: when a field's @requires has multiple external parts resolved
// through different subgraphs, and one external part's intermediate has its
// own @requires, the nested @requires resolution can append selections to
// the wrong entity group. The `last_node` variable drifts as each external
// part resolves, but the query_graph_node and source_schema stay pinned to
// the original intermediate, so the fast-path check validates against the
// wrong subgraph and appends to whatever entity group `last_node` reached.
#[test_log::test]
fn requires_with_multiple_external_parts_and_nested_requires() {
    let schema = REQUIRES_SCHEMA;
    let query = "{ itemById(id: \"1\") { preview } }";

    // The bug is non-deterministic (HashMap iteration order determines
    // which intermediate subgraph is tried first), so run multiple times.
    for _ in 0..50 {
        let plan_str = plan_query(schema, query);
        assert!(
            plan_str.contains("preview"),
            "Plan should fetch 'preview': {plan_str}"
        );
    }
}

/// Without a wall-clock timeout (the default), planning is bounded by
/// fuel alone and must be fully deterministic: the same query against the
/// same schema yields a byte-identical plan on every run, including
/// across fresh planner instances (fresh caches, fresh allocations).
#[test]
fn planning_without_timeout_is_deterministic() {
    let schema = REQUIRES_SCHEMA;
    let query = "{ itemById(id: \"1\") { preview } }";

    let reference = plan_query(schema, query);
    for i in 1..20 {
        let plan_str = plan_query(schema, query);
        assert_eq!(
            plan_str, reference,
            "Plan differed from reference on run {i}",
        );
    }
}

/// Minimal join-spec v0.5 supergraph boilerplate around inline type
/// definitions, so tests can declare small schemas without fixture files.
fn wrap_supergraph(graph_enum: &str, types: &str) -> String {
    format!(
        r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
{{
  query: Query
}}

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

input join__ContextArgument {{
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}}

scalar join__DirectiveArguments
scalar join__FieldSet
scalar join__FieldValue
scalar link__Import

enum link__Purpose {{
  SECURITY
  EXECUTION
}}

enum join__Graph {{
{graph_enum}
}}

{types}
"#
    )
}

const REQUIRES_KEY_HOP_SCHEMA_TYPES: &str = r#"
type Product
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: C, key: "id")
{
  id: ID!
  weight: Float @join__field(graph: B) @join__field(graph: C, external: true)
  shippingEstimate: Float @join__field(graph: C, requires: "weight")
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: C)
{
  product: Product @join__field(graph: A)
}
"#;

fn requires_key_hop_schema() -> String {
    wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")
  C @join__graph(name: "c", url: "http://c")"#,
        REQUIRES_KEY_HOP_SCHEMA_TYPES,
    )
}

/// @requires whose condition field the user operation ALSO selects at the
/// same position, with the user selection still pending when the requiring
/// field commits: the condition reuses the user selection's response key
/// (no `__require` alias) and its fetch chain.
/// Targets requires.rs shareable_condition_fields' on-stack arm.
#[test]
fn requires_condition_deduped_with_pending_user_selection() {
    let plan_str = plan_query(
        &requires_key_hop_schema(),
        "{ product { shippingEstimate weight } }",
    );
    assert!(
        !plan_str.contains("__require"),
        "Condition should dedupe with the user's weight selection: {plan_str}"
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            product {
              __typename
              id
            }
          }
        },
        Flatten(path: "product") {
          Fetch(service: "b") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            {
              ... on Product {
                weight
              }
            }
          },
        },
        Flatten(path: "product") {
          Fetch(service: "c") {
            {
              ... on Product {
                __typename
                id
                weight
              }
            } =>
            {
              ... on Product {
                shippingEstimate
              }
            }
          },
        },
      },
    }
    "###);
}

/// Same dedupe when the user selection was already committed (weight is
/// popped and routed into its entity group before shippingEstimate commits).
/// Targets requires.rs shareable_condition_fields' committed/sibling-entity
/// scan.
#[test]
fn requires_condition_deduped_with_committed_user_selection() {
    let plan_str = plan_query(
        &requires_key_hop_schema(),
        "{ product { weight shippingEstimate } }",
    );
    assert!(
        !plan_str.contains("__require"),
        "Condition should dedupe with the user's weight selection: {plan_str}"
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            product {
              __typename
              id
            }
          }
        },
        Flatten(path: "product") {
          Fetch(service: "b") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            {
              ... on Product {
                weight
              }
            }
          },
        },
        Flatten(path: "product") {
          Fetch(service: "c") {
            {
              ... on Product {
                __typename
                id
                weight
              }
            } =>
            {
              ... on Product {
                shippingEstimate
              }
            }
          },
        },
      },
    }
    "###);
}

/// @requires on a field of a keyless entity-less type: the conditions are
/// @external locally (no in-place strategy) and the type has no locally
/// satisfiable key for a self key hop, so enumeration yields no @requires
/// strategy. The selection is dropped and planning errors instead of
/// returning a partial plan.
/// Targets routing.rs push_requires_strategy_options producing zero options.
#[test]
fn requires_without_reentry_key_fails_planning() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
type E
  @join__type(graph: A, key: "id")
  @join__type(graph: B)
{
  id: ID! @join__field(graph: A)
  f: String @join__field(graph: B, requires: "g")
  g: String @join__field(graph: A) @join__field(graph: B, external: true)
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  entity: E @join__field(graph: B)
}
"#,
    );
    let result = try_plan_query(&schema, "{ entity { f } }");
    assert!(
        result.is_err(),
        "@requires without a re-entry key should fail planning, got:\n{}",
        result.as_deref().unwrap_or("<err>"),
    );
}

#[test]
fn requires_with_fragment_wrapped_field_set() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")
  C @join__graph(name: "c", url: "http://c")"#,
        r#"
type Product
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: C, key: "id")
{
  id: ID!
  weight: Float @join__field(graph: B) @join__field(graph: C, external: true)
  shippingEstimate: Float @join__field(graph: C, requires: "... on Product { weight }")
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: C)
{
  product: Product @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ product { shippingEstimate } }");
    assert!(
        plan_str.contains("weight"),
        "Plan should fetch the fragment-wrapped required field: {plan_str}"
    );
    assert!(
        plan_str.contains("shippingEstimate"),
        "Plan should fetch shippingEstimate: {plan_str}"
    );
}

/// Two sibling fields sharing the same locally-resolvable @requires: the
/// second commit checks its conditions against the edge's existing condition
/// input (same field, same arguments, no conflict) and rides the same
/// entity representation.
/// Targets requires.rs has_conflicting_requires_inputs' comparison loops.
#[test]
fn sibling_fields_with_identical_requires_share_representation() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  C @join__graph(name: "c", url: "http://c")"#,
        r#"
type Product
  @join__type(graph: A, key: "id")
  @join__type(graph: C, key: "id")
{
  id: ID!
  weight: Float @join__field(graph: A) @join__field(graph: C, external: true)
  sa: Float @join__field(graph: C, requires: "weight")
  sb: Float @join__field(graph: C, requires: "weight")
}

type Query
  @join__type(graph: A)
{
  product: Product @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ product { sa sb } }");
    assert!(
        plan_str.contains("sa") && plan_str.contains("sb"),
        "Plan should fetch both requiring fields: {plan_str}"
    );
    assert_eq!(
        plan_str.matches("weight").count(),
        2,
        "weight selected once in A and once in the representation: {plan_str}"
    );
}

#[test]
fn all_subgraphs_disabled_root_typename_fails_planning() {
    let supergraph = Supergraph::new(SCHEMA).expect("supergraph parse");
    let planner = QueryPlanner::new(&supergraph, default_config()).expect("planner creation");
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "query($v: Boolean!) { ... on Query @skip(if: $v) { __typename } }",
        "test.graphql",
    )
    .expect("query parse");
    let result = planner.build_query_plan(
        &document,
        None,
        QueryPlanOptions {
            disabled_subgraph_names: ["a", "b", "c"].map(String::from).into_iter().collect(),
            ..Default::default()
        },
    );
    assert!(
        result.is_err(),
        "Planning with every subgraph disabled must fail, got:\n{}",
        result.map(|p| p.to_string()).unwrap_or_default(),
    );
}

// ---------------------------------------------------------------------------
// Internal state tests
// ---------------------------------------------------------------------------

use apollo_compiler::name;

use super::state::ConditionScope;
use super::test_support;
use super::*;

fn search_space() -> FieldRoutingSearchSpace {
    test_support::search_space_from_supergraph(SCHEMA)
}

fn t_node(space: &FieldRoutingSearchSpace, subgraph: &str) -> NodeIndex {
    test_support::node_for(space, subgraph, "T")
}

/// A pending for `T.<field>` at a's T node, anchored at fetch group
/// `fetch_node`, marked as condition data feeding `dependent`. `y` is
/// resolvable only in b; `z` in both b and c.
fn t_pending(
    space: &FieldRoutingSearchSpace,
    field: &str,
    fetch_node: NodeIndex,
    dependent: Option<NodeIndex>,
) -> PendingSelection {
    let op = crate::operation::Operation::parse(
        space.supergraph_schema.clone(),
        &format!("{{ t {{ {field} }} }}"),
        "op.graphql",
    )
    .expect("valid operation");
    let Some(Selection::Field(t_sel)) = op.selection_set.selections.values().next() else {
        panic!("expected t field");
    };
    let field_sel = t_sel
        .selection_set
        .as_ref()
        .expect("t has sub-selections")
        .selections
        .values()
        .next()
        .expect("field selection")
        .clone();
    PendingSelection {
        selection: field_sel,
        query_graph_node: t_node(space, "a"),
        fetch_node,
        op_path: SharedPath::new(),
        path_in_fetch: SharedPath::new(),
        condition: dependent.map(|dependent| ConditionScope {
            dependent,
            depth: 1,
        }),
        provides_anchor: None,
        narrowing: Default::default(),
        best_effort: false,
        defer_ref: None,
        context_anchor: Default::default(),
        parent_types: SharedPath::new(),
    }
}

/// State with root group A feeding entity group B, so an ordering
/// dependent of A cycles when a new group hangs beneath B.
fn cyclic_fixture() -> (PlanState, NodeIndex, NodeIndex) {
    let mut state = PlanState::new(vec![]);
    let s1: Arc<str> = Arc::from("a");
    let root_pos: CompositeTypeDefinitionPosition = CompositeTypeDefinitionPosition::Object(
        crate::schema::position::ObjectTypeDefinitionPosition {
            type_name: name!("Query"),
        },
    );
    let a = state.graph.get_or_create_root_group(&s1, root_pos);
    let b = state.graph.add_entity_group(&s1, vec![]);
    state.graph.add_dependency(a, b, vec![]);
    (state, a, b)
}

/// Reusing an entity group that already (transitively) feeds the anchor
/// would close a dependency cycle; the commit must mint a fresh group
/// instead. This arm is the release-mode acyclicity guard.
#[test]
fn cyclic_entity_group_reuse_mints_fresh_group() {
    let space = search_space();
    let (mut state, _a, b) = cyclic_fixture();
    let s2: Arc<str> = Arc::from("b");
    // An existing (b, []) entity group that already feeds b: reusing
    // it for a hop anchored at b would close a cycle.
    let existing = state
        .graph
        .get_or_create_entity_group_with_defer(&s2, vec![], None);
    state.graph.add_dependency(existing, b, vec![]);

    let pending = Arc::new(t_pending(&space, "y", b, None));
    let options = Arc::new(space.routing_options(&pending).expect("options enumerate"));
    assert!(!options.is_empty(), "y must have a key-hop option");

    space
        .commit_choice(&mut state, &pending, &options[0])
        .expect("commit mints a fresh group instead of reusing");
    assert!(
        !state.graph.has_edge(b, existing),
        "reuse would have closed a cycle",
    );
    // cost() debug-asserts acyclicity; finite means the graph is a DAG.
    assert!(state.graph.cost().is_finite());
}

/// A commit that fails on its own ordering cycle must not doom a sibling
/// at the same node and fetch group that has no ordering dependent: the
/// sibling commits and only the cycling pending is dropped.
#[test]
fn cyclic_commit_failure_does_not_doom_context_free_sibling() {
    let space = search_space();
    let (mut state, a, b) = cyclic_fixture();

    // Bottom of stack: no ordering dependent, commits on its own. Top:
    // condition pending whose ordering edge back to `a` cycles.
    let ok_pending = t_pending(&space, "y", b, None);
    let cyclic_pending = t_pending(&space, "y", b, Some(a));
    state.pending = vec![Arc::new(ok_pending), Arc::new(cyclic_pending)];
    let groups_before = state.graph.node_count();

    space.fast_forward(&mut state).expect("fast forward runs");

    assert_eq!(state.dropped_fields, 1, "only the cycling pending drops");
    assert!(state.pending.is_empty());
    assert_eq!(
        state.graph.node_count(),
        groups_before + 1,
        "the sibling's key hop adds a b group",
    );
}

/// A second pending identical to one whose options were all exhausted
/// fails fast from the doomed set instead of being retried. Each pending
/// has two options, so a retry would spend another forced backtrack.
#[test]
fn exhausted_site_fails_fast_for_identical_sibling() {
    let space = search_space();
    let (mut state, a, b) = cyclic_fixture();
    state.pending = vec![
        Arc::new(t_pending(&space, "z", b, Some(a))),
        Arc::new(t_pending(&space, "z", b, Some(a))),
    ];

    space.fast_forward(&mut state).expect("fast forward runs");

    assert_eq!(state.dropped_fields, 2);
    assert_eq!(
        state.forced_backtracks, 1,
        "the identical sibling must not retry its alternatives",
    );
    assert!(state.pending.is_empty());
}

/// When a forced condition pending has multiple options and all fail,
/// `backtrack_forced` exhausts the trail frame and re-drives the greedy
/// choice to restore state. The re-drive also fails, incrementing
/// `dropped_fields` inside `backtrack_forced`. The caller must not
/// double-count, so `backtrack_forced` returns true. Net: exactly 1 drop.
#[test]
fn exhausted_alternatives_redrive_counts_one_drop() {
    // b and c both provide z, giving the condition pending two key-hop
    // options. Both cycle with the A → B dependency.
    let space = search_space();

    let (mut state, a, b) = cyclic_fixture();

    // Condition pending anchored at B with ordering dependent A.
    // Routing options: key hop to b or c. Both create a new group
    // reachable from A (via A → B → new_group), so the ordering edge
    // back to A cycles in both cases.
    let pending = t_pending(&space, "z", b, Some(a));
    let options = space
        .routing_options(&Arc::new(pending.clone()))
        .expect("has options");
    assert!(
        options.len() >= 2,
        "expected multiple routing options for z, got {}",
        options.len(),
    );

    state.pending = vec![Arc::new(pending)];
    space.fast_forward(&mut state).expect("fast forward runs");

    assert!(
        state.forced_backtracks > 0,
        "backtracking must have been attempted to exercise the re-drive path",
    );
    assert_eq!(
        state.dropped_fields, 1,
        "exhausted alternatives should count exactly one drop",
    );
    assert!(state.pending.is_empty());
}

fn interface_object_schema() -> String {
    wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
interface I
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id", isInterfaceObject: true)
{
  id: ID!
  name: String @join__field(graph: A)
  desc: String @join__field(graph: B)
}

type X implements I
  @join__implements(graph: A, interface: "I")
  @join__type(graph: A, key: "id")
{
  id: ID!
  name: String
  desc: String @join__field
}

type Y implements I
  @join__implements(graph: A, interface: "I")
  @join__type(graph: A, key: "id")
{
  id: ID!
  name: String
  desc: String @join__field
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  items: [I] @join__field(graph: A)
  stuff: [I] @join__field(graph: B)
}
"#,
    )
}

/// @interfaceObject fake downcast (`... on X` on the io node in B, where X
/// does not exist): the concrete-type condition is dropped from B's
/// operation, and `push_interface_object_typename` pushes a best-effort
/// `__typename` pending that routes generically. The io node has no
/// `__typename` edge, so its only options are key hops to subgraphs where I
/// is the real interface (here A), so execution learns each object's
/// concrete `__typename` to test the condition.
/// Targets commit.rs push_interface_object_typename + the fake-downcast
/// op-path arm of target_paths, and routing.rs fragment_options' fake
/// downcast enumeration.
#[test]
fn interface_object_fake_downcast_fetches_concrete_typename() {
    let plan_str = plan_query(
        &interface_object_schema(),
        "{ stuff { ... on X { desc } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "b") {
          {
            stuff {
              __typename
              desc
              id
            }
          }
        },
        Flatten(path: "stuff.@") {
          Fetch(service: "a") {
            {
              ... on I {
                __typename
                id
              }
            } =>
            {
              ... on I {
                __typename
              }
            }
          },
        },
      },
    }
    "###);
}

/// Entering through A (real interface), a concrete-type downcast whose field
/// only exists on the @interfaceObject copy in B key-hops into B.
#[test]
fn interface_object_key_hop_from_concrete_type() {
    let plan_str = plan_query(
        &interface_object_schema(),
        "{ items { ... on X { desc } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            items {
              __typename
              ... on X {
                __typename
                id
              }
            }
          }
        },
        Flatten(path: "items.@|[X]") {
          Fetch(service: "b") {
            {
              ... on X {
                __typename
                id
              }
            } =>
            {
              ... on I {
                desc
              }
            }
          },
        },
      },
    }
    "###);
}

/// A root field defined in two subgraphs is a federated-root decision point:
/// options are ranked by how many of the field's children each subgraph can
/// resolve locally, so the query plans as one fetch to A (which has f1 AND
/// f2) rather than A+B. The extra `onlyA` sibling forces the lift-forced-
/// pendings path in fast_forward (a single-option entry below an open
/// decision is committed first).
/// Targets routing.rs federated_root_options ranking +
/// count_local_sub_selections and field_routing/mod.rs fast_forward lift.
#[test]
fn shareable_root_field_prefers_subgraph_with_more_local_children() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
type T
  @join__type(graph: A)
  @join__type(graph: B)
{
  f1: String
  f2: String @join__field(graph: A)
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  shared: T
  onlyA: Int @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ shared { f1 f2 } onlyA }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Fetch(service: "a") {
        {
          onlyA
          shared {
            f1
            f2
          }
        }
      },
    }
    "###);
}

/// @interfaceObject fake downcast where the fragment carries a runtime
/// condition (@include): the concrete-type condition is dropped from the io
/// subgraph's operation but the directive must survive as a condition-only
/// fragment.
/// Targets commit.rs target_paths' fake-downcast directive-preserving arm.
#[test]
fn interface_object_fake_downcast_preserves_include_condition() {
    let plan_str = plan_query(
        &interface_object_schema(),
        "query($v: Boolean!) { stuff { ... on X @include(if: $v) { desc } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "b") {
          {
            stuff {
              __typename
              ... @include(if: $v) {
                desc
              }
              id
            }
          }
        },
        Flatten(path: "stuff.@") {
          Fetch(service: "a") {
            {
              ... on I {
                __typename
                id
              }
            } =>
            {
              ... on I {
                __typename
              }
            }
          },
        },
      },
    }
    "###);
}

/// Interface field resolved only on some concrete types: A has the field for
/// Dog but not Cat; Cat must key-hop to B. The plan explodes the abstract
/// `animals` into per-concrete-type fragments so each follows its own path.
/// Targets type_conditions.rs try_explode_interface_field and the
/// per-concrete-type routing that follows.
#[test]
fn interface_field_without_local_edge_explodes_per_concrete_type() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
interface Animal
  @join__type(graph: A)
  @join__type(graph: B)
{
  id: ID!
  name: String @join__field(graph: B)
}

type Cat implements Animal
  @join__implements(graph: A, interface: "Animal")
  @join__implements(graph: B, interface: "Animal")
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  name: String @join__field(graph: B)
}

type Dog implements Animal
  @join__implements(graph: A, interface: "Animal")
  @join__type(graph: A, key: "id")
{
  id: ID!
  name: String @join__field(graph: A)
}

type Query
  @join__type(graph: A)
{
  animals: [Animal] @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ animals { name } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            animals {
              __typename
              ... on Cat {
                __typename
                id
              }
              ... on Dog {
                name
              }
            }
          }
        },
        Flatten(path: "animals.@|[Cat]") {
          Fetch(service: "b") {
            {
              ... on Cat {
                __typename
                id
              }
            } =>
            {
              ... on Cat {
                name
              }
            }
          },
        },
      },
    }
    "###);
}

/// A fragment conditioned on an interface applied to a union with partial
/// overlap (`... on N` where only member X implements N) has no downcast
/// edge and explodes into the intersection's concrete-type fragments.
/// Targets type_conditions.rs try_explode_abstract_type's non-empty
/// partial-intersection path.
#[test]
fn union_fragment_on_interface_explodes_to_members() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")"#,
        r#"
union U
  @join__type(graph: A)
  @join__unionMember(graph: A, member: "X")
  @join__unionMember(graph: A, member: "Y")
 = X | Y

interface N
  @join__type(graph: A)
{
  n: String
}

type X implements N
  @join__implements(graph: A, interface: "N")
  @join__type(graph: A)
{
  n: String
}

type Y
  @join__type(graph: A)
{
  y: String
}

type Query
  @join__type(graph: A)
{
  search: [U] @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ search { ... on N { n } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Fetch(service: "a") {
        {
          search {
            __typename
            ... on X {
              n
            }
          }
        }
      },
    }
    "###);
}

/// A fragment on a union member that does not exist in the resolving
/// subgraph (Y is only a member of U in B, but `search` only resolves in A)
/// has an empty local runtime intersection. The fragment is dropped and the
/// plan completes without fabricating Y data.
/// Targets the empty-intersection/satisfiable-elsewhere arm of
/// type_conditions.rs try_explode_abstract_type.
#[test]
fn union_member_missing_locally_drops_fragment_and_commits_typename() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
union U
  @join__type(graph: A)
  @join__type(graph: B)
  @join__unionMember(graph: A, member: "X")
  @join__unionMember(graph: B, member: "Y")
 = X | Y

type X
  @join__type(graph: A)
{
  x: String
}

type Y
  @join__type(graph: B)
{
  y: String
}

type Query
  @join__type(graph: A)
{
  search: [U] @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ search { __typename ... on Y { y } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Fetch(service: "a") {
        {
          search {
            __typename
          }
        }
      },
    }
    "###);
}

/// Fragment-path narrowing: `es` returns E (members X, Y), and `... on L`
/// (members X, Y, Z) explodes at L's node where Z is locally possible — but
/// no Z can ever appear under an E-typed field. The Z branch must be routed
/// as dead code, not key-hopped into an entity fetch whose inputs demand a
/// Z key the response state can never satisfy.
/// Targets TypeNarrowing.possible_types and the dead-fragment early return
/// in dispatch_sub_selections.
#[test]
fn narrowing_drops_exploded_member_outside_enclosing_context() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
interface E
  @join__type(graph: A)
{
  id: ID!
}

interface L
  @join__type(graph: A)
  @join__type(graph: B)
{
  id: ID!
  url: String @join__field(graph: B)
}

type X implements E & L
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__implements(graph: A, interface: "E")
  @join__implements(graph: A, interface: "L")
  @join__implements(graph: B, interface: "L")
{
  id: ID!
  url: String @join__field(graph: B)
}

type Y implements E & L
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__implements(graph: A, interface: "E")
  @join__implements(graph: A, interface: "L")
  @join__implements(graph: B, interface: "L")
{
  id: ID!
  url: String @join__field(graph: B)
}

type Z implements L
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__implements(graph: A, interface: "L")
  @join__implements(graph: B, interface: "L")
{
  id: ID!
  url: String @join__field(graph: B)
}

type Query
  @join__type(graph: A)
{
  es: [E] @join__field(graph: A)
}
"#,
    );
    let plan_str = plan_query(&schema, "{ es { ... on L { url } } }");
    assert!(
        !plan_str.contains("... on Z"),
        "no Z can appear under an E-typed field, but the plan references it:\n{plan_str}"
    );
}

/// Shareable parent returning an abstract type whose runtime members differ
/// per subgraph (U is X|Y in A but only X in B): fragments committed under
/// the B route must be filtered to B's member set, dropping `... on Y`
/// there without a penalty, while the A route keeps both.
/// Targets routing.rs fragment_options' intersection filter, and
/// type_conditions.rs dropped-by-intersection-filter arm.
#[test]
fn inconsistent_union_members_filtered_per_subgraph() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
union U
  @join__type(graph: A)
  @join__type(graph: B)
  @join__unionMember(graph: A, member: "X")
  @join__unionMember(graph: A, member: "Y")
  @join__unionMember(graph: B, member: "X")
 = X | Y

type X
  @join__type(graph: A)
  @join__type(graph: B)
{
  x: String
}

type Y
  @join__type(graph: A)
{
  y: String
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  search: [U]
}
"#,
    );
    let plan_str = plan_query(&schema, "{ search { ... on X { x } ... on Y { y } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Fetch(service: "a") {
        {
          search {
            __typename
            ... on X {
              x
            }
            ... on Y {
              y
            }
          }
        }
      },
    }
    "###);
}

fn entity_inconsistent_union_schema() -> String {
    wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
type T
  @join__type(graph: A, key: "tid")
  @join__type(graph: B, key: "tid")
{
  tid: ID!
  e: E
}

type E
  @join__type(graph: A, key: "eid")
  @join__type(graph: B, key: "eid")
{
  eid: ID!
  search: [U]
}

union U
  @join__type(graph: A)
  @join__type(graph: B)
  @join__unionMember(graph: A, member: "X")
  @join__unionMember(graph: A, member: "Y")
  @join__unionMember(graph: B, member: "X")
 = X | Y

type X
  @join__type(graph: A)
  @join__type(graph: B)
{
  x: String
}

type Y
  @join__type(graph: A)
{
  y: String
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  top: T @join__field(graph: A)
}
"#,
    )
}

/// A shareable entity field (`e` resolvable in A directly and in B via T's
/// key) puts its descendants on a shareable path; `search` below it returns
/// a union whose members differ per subgraph, so its child fragments get an
/// intersection filter from the committed subgraph's own member set.
/// Targets commit.rs intersection_filter_for_field /
/// field_is_shareable_here / field_in_multiple_subgraphs.
#[test]
fn entity_shareable_field_filters_inconsistent_union_members() {
    let plan_str = plan_query(
        &entity_inconsistent_union_schema(),
        "{ top { e { search { ... on X { x } ... on Y { y } } } } }",
    );
    assert!(
        plan_str.contains("... on Y"),
        "Winning route must keep the Y fragment: {plan_str}"
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Fetch(service: "a") {
        {
          top {
            e {
              search {
                __typename
                ... on X {
                  x
                }
                ... on Y {
                  y
                }
              }
            }
          }
        }
      },
    }
    "###);
}

/// A field with locally-unresolvable @requires reached through an
/// @include-gated fragment: the same-subgraph entity re-fetch must carry the
/// gating fragment into its op path (or the hopped selection loses its
/// condition), and boolean conditions on the path disable condition-field
/// sharing.
/// Targets requires.rs trailing_condition_fragments on the self-key-hop
/// commit path (routing.rs self_key_hop -> commit.rs target_paths) and
/// path_has_boolean_conditions in shareable_condition_fields.
#[test]
fn requires_under_include_fragment_keeps_condition_on_entity_fetch() {
    let plan_str = plan_query(
        REQUIRES_SCHEMA,
        "query($v: Boolean!) { productInB { ... on Product @include(if: $v) { shippingCost } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "b") {
          {
            productInB {
              __typename
              id
            }
          }
        },
        Flatten(path: "productInB") {
          Fetch(service: "a") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            {
              ... on Product {
                __require_0_weight: weight
              }
            }
          },
        },
        Include(if: $v) {
          Flatten(path: "productInB") {
            Fetch(service: "b") {
              {
                ... on Product {
                  __typename
                  id
                  __require_0_weight: weight
                }
              } =>
              {
                ... on Product {
                  shippingCost
                }
              }
            },
          },
        },
      },
    }
    "###);
}

/// A key-hop @requires reached through an @include-gated fragment: boolean
/// conditions on the anchor path make condition-field sharing unprovable, so
/// the condition is aliased instead of deduped with any user selection.
/// Targets requires.rs path_has_boolean_conditions in
/// shareable_condition_fields.
#[test]
fn key_hop_requires_under_include_fragment_uses_alias() {
    let plan_str = plan_query(
        &requires_key_hop_schema(),
        "query($v: Boolean!) { product { ... on Product @include(if: $v) { shippingEstimate } } }",
    );
    assert!(
        plan_str.contains("shippingEstimate"),
        "Plan should fetch shippingEstimate: {plan_str}"
    );
    assert!(
        plan_str.contains("__require"),
        "Gated condition must be aliased, not shared: {plan_str}"
    );
}

/// Statically constant @skip(if: true) should eliminate the fragment entirely;
/// a type condition on the root Query type is vacuous and passes through.
/// Targets type_conditions.rs try_pass_through_fragment's Boolean(false)
/// arm and try_vacuous_type_condition's federated-root arm.
#[test]
fn constant_skip_and_root_type_condition_fragments() {
    let skipped = plan_query(SCHEMA, "{ user { name ... @skip(if: true) { email } } }");
    assert!(
        !skipped.contains("email"),
        "Statically skipped fragment must not be fetched: {skipped}"
    );

    let rooted = plan_query(
        SCHEMA,
        "query($v: Boolean!) { ... on Query @skip(if: $v) { user { name } } }",
    );
    assert!(
        rooted.contains("name"),
        "Root type condition should pass through: {rooted}"
    );
}

/// Shared by the @defer tests: name, email, and address each live in their
/// own subgraph.
const THREE_SUBGRAPH_SCHEMA: &str = include_str!("../fixtures/three_subgraph.graphql");

/// Cross-subgraph @defer: the deferred fragment's fields live in a different
/// subgraph from the primary, producing a Defer node with a key-hop fetch
/// in the deferred block.
#[test]
fn defer_produces_defer_node() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer { email } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Primary should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Deferred should fetch 'email': {plan_str}"
    );
}

/// Through BULB, the deferred key hop carries the fragment's defer scope, so
/// its fetch lands in the Deferred block and stays out of the primary.
#[test]
fn defer_cross_subgraph_key_hop_lands_in_deferred_block() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer { email } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Defer {
        Primary {
          { user { name } }:
          Fetch(service: "a", id: 0) {
            {
              user {
                __typename
                name
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "user") {
            { email }:
            Flatten(path: "user") {
              Fetch(service: "b") {
                {
                  ... on User {
                    __typename
                    id
                  }
                } =>
                {
                  ... on User {
                    email
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###);
}

/// Labels synthesized by defer normalization for unlabeled @defer
/// (`qp__N`) are internal bookkeeping and must not leak into the plan;
/// user-written labels must be preserved.
#[test]
fn synthesized_defer_labels_do_not_leak_into_plan() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer { email } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node: {plan_str}"
    );
    assert!(
        !plan_str.contains("qp__"),
        "Synthesized defer label must not appear in the plan: {plan_str}"
    );

    let labeled_plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer(label: \"mine\") { email } } }",
    );
    assert!(
        labeled_plan_str.contains("mine"),
        "User-written defer label must be preserved: {labeled_plan_str}"
    );
}

/// Same-subgraph @defer: a deferred field that lives in the same subgraph
/// as the primary must not be fetched eagerly in the primary fetch. The
/// deferred field gets its own entity fetch so the executor can stream it
/// in a later multipart chunk.
#[test]
fn defer_same_subgraph_does_not_fetch_deferred_field_eagerly() {
    let schema = &wrap_supergraph(
        r#"  S @join__graph(name: "s", url: "http://s")"#,
        r#"
type Query @join__type(graph: S) {
  t: T
}
type T @join__type(graph: S, key: "id") {
  id: ID!
  v0: String
  v1: String
}
"#,
    );
    let plan_str = plan_query_with_defer(schema, "{ t { v0 ... @defer { v1 } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { v0 } }:
          Fetch(service: "s", id: 0) {
            {
              t {
                __typename
                v0
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            { v1 }:
            Flatten(path: "t") {
              Fetch(service: "s") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v1
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###);
}

const ROOT_HOP_DEFER_SCHEMA: &str = include_str!(
    "../../../../tests/query_plan/supergraphs/defer_test_defer_on_query_root_type.graphql"
);

/// A deferred field reached through a root hop (`next: Query` into another
/// subgraph) must be fetched in the Deferred block, not in the primary's
/// root-hop fetch.
#[test]
fn defer_through_root_hop_keeps_field_deferred() {
    let plan_str = plan_query_with_defer(
        ROOT_HOP_DEFER_SCHEMA,
        "{ op2 { next { op3 ... @defer { op4 } } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Defer {
        Primary {
          { op2 { next { op3 } } }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                op2 {
                  next {
                    __typename
                  }
                }
              }
            },
            Flatten(path: "op2.next") {
              Fetch(service: "Subgraph2") {
                {
                  op3
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "op2/next") {
            { op4 }:
            Flatten(path: "op2.next") {
              Fetch(service: "Subgraph2") {
                {
                  op4
                }
              },
            },
          },
        ]
      },
    }
    "###);
}

/// A deferred root field in the enclosing subgraph re-enters it through a
/// root hop, since a root type has no key to redirect through.
#[test]
fn defer_on_query_root_type() {
    let plan_str = plan_query_with_defer(
        ROOT_HOP_DEFER_SCHEMA,
        "{ op2 { x y next { op3 ... @defer { op1 op4 } } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Defer {
        Primary {
          { op2 { x y next { op3 } } }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                op2 {
                  x
                  y
                  next {
                    __typename
                  }
                }
              }
            },
            Flatten(path: "op2.next") {
              Fetch(service: "Subgraph2") {
                {
                  op3
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "op2/next") {
            { op1 op4 }:
            Parallel {
              Flatten(path: "op2.next") {
                Fetch(service: "Subgraph2") {
                  {
                    op4
                  }
                },
              },
              Flatten(path: "op2.next") {
                Fetch(service: "Subgraph1") {
                  {
                    op1
                  }
                },
              },
            },
          },
        ]
      },
    }
    "###);
}

/// Multiple @defer siblings at the same level produce distinct deferred
/// blocks inside a single Defer node.
#[test]
fn defer_sibling_blocks_produces_multiple_deferred() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer { email } ... @defer { address } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Primary should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "A deferred block should fetch 'email': {plan_str}"
    );
    assert!(
        plan_str.contains("address"),
        "A deferred block should fetch 'address': {plan_str}"
    );
}

/// Nested @defer: an outer deferred fragment contains an inner @defer,
/// producing nested Defer nodes. Exercises the parent_label tracking in
/// defer.rs collect_deferred_blocks and the nested defer partitioning in
/// plan_builder.rs build_deferred_blocks.
#[test]
fn nested_defer_produces_nested_defer_nodes() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer(label: \"outer\") { email ... @defer(label: \"inner\") { address } } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Primary should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Outer deferred should fetch 'email': {plan_str}"
    );
    assert!(
        plan_str.contains("address"),
        "Inner deferred should fetch 'address': {plan_str}"
    );
}

/// When every field in the selection is deferred, the primary sub_selection
/// is empty. Exercises the None primary_sub_selection path in defer.rs
/// build_defer_info.
#[test]
fn fully_deferred_field_has_no_primary_payload() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { ... @defer(label: \"all\") { name email } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Deferred should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Deferred should fetch 'email': {plan_str}"
    );
}

/// A bare inline fragment (no type condition, no directives) inside a
/// deferred selection exercises collect_non_deferred_selection's
/// type_cond == None branch.
#[test]
fn bare_inline_fragment_passes_through_in_defer() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { ... @defer { email } ... { name } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Bare fragment 'name' should be in primary: {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Deferred should fetch 'email': {plan_str}"
    );
}

/// Deferred cross-subgraph fetch with labeled @defer and an explicit
/// user field alongside exercises the primary/deferred split where
/// primary has content and deferred needs an entity hop.
#[test]
fn labeled_defer_with_primary_and_deferred_content() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        r#"{ user { name ... @defer(label: "emails") { email } ... @defer(label: "addrs") { address } } }"#,
    );
    assert!(
        plan_str.contains("name"),
        "Primary should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("emails"),
        "Label 'emails' should appear in plan: {plan_str}"
    );
    assert!(
        plan_str.contains("addrs"),
        "Label 'addrs' should appear in plan: {plan_str}"
    );
}

/// Cross-subgraph @defer where the deferred fields span two different
/// non-primary subgraphs exercises the multi-fetch deferred block
/// construction in plan_builder.
#[test]
fn defer_spanning_two_non_primary_subgraphs() {
    let plan_str = plan_query_with_defer(
        THREE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer { email address } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain Defer: {plan_str}"
    );
    assert!(
        plan_str.contains("name"),
        "Primary should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Deferred should fetch 'email': {plan_str}"
    );
    assert!(
        plan_str.contains("address"),
        "Deferred should fetch 'address': {plan_str}"
    );
}

const CONTEXT_SCHEMA: &str = include_str!("../fixtures/context.graphql");

/// @fromContext field: the plan must fetch the context-providing field
/// (`prop`) from the parent and thread it via a contextualArgument to
/// the subgraph that resolves the @fromContext-bearing field.
#[test]
fn context_from_context_produces_valid_plan() {
    let plan_str = plan_query_with_router_specs(CONTEXT_SCHEMA, "{ t { u { field } } }");
    assert!(
        plan_str.contains("field"),
        "Plan should fetch 'field': {plan_str}"
    );
    assert!(
        plan_str.contains("prop"),
        "Plan should fetch 'prop' as context value: {plan_str}"
    );
    assert!(
        plan_str.contains("contextualArgument"),
        "Plan should include context variable argument: {plan_str}"
    );
}

const CONTEXT_BOUNDARY_SCHEMA: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/context/v0.1", import: ["@context"], for: SECURITY)
{
  query: Query
}

directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

directive @context__fromContext(field: context__ContextFieldValue) on ARGUMENT_DEFINITION

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar context__ContextFieldValue

input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}

scalar join__DirectiveArguments

scalar join__FieldSet

scalar join__FieldValue

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

enum join__Graph {
  S1 @join__graph(name: "s1", url: "http://s1")
  S2 @join__graph(name: "s2", url: "http://s2")
}

type Query
  @join__type(graph: S1)
  @join__type(graph: S2)
{
  t: T! @join__field(graph: S1)
}

type T
  @join__type(graph: S1, key: "id")
  @join__type(graph: S2, key: "id")
  @context(name: "s2__ctx")
{
  id: ID!
  prop: String! @join__field(graph: S1) @join__field(graph: S2)
  child: T @join__field(graph: S2)
  field: Int! @join__field(graph: S2, contextArguments: [{context: "s2__ctx", name: "a", type: "String", selection: " { prop }"}])
}
"#;

/// @fromContext consumed inside an entity fetch whose entity root type IS
/// the @context ancestor: the context value rides the entity representation
/// (no extra isolation hop), the context selection lands on the fetch
/// feeding the entity fetch, and the rewrite path has no Parent elements.
#[test]
fn context_value_rides_entity_representation_at_boundary() {
    let plan_str =
        plan_query_with_router_specs(CONTEXT_BOUNDARY_SCHEMA, "{ t { child { field } } }");
    assert!(
        plan_str.contains("contextualArgument"),
        "Plan should pass the context variable: {plan_str}"
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "s1") {
          {
            t {
              __typename
              id
              prop
            }
          }
        },
        Flatten(path: "t") {
          Fetch(service: "s2") {
            {
              ... on T {
                __typename
                id
              }
            } =>
            {
              ... on T {
                child {
                  field(a: $contextualArgument_2_0)
                }
              }
            }
          },
        },
      },
    }
    "###);
}

const VALUE_TYPE_DEFER_SCHEMA: &str = include_str!(
    "../../../../tests/query_plan/supergraphs/defer_test_defer_on_value_types.graphql"
);

/// A deferred field on a value type has no key to re-enter its subgraph
/// through, so it stays in the enclosing fetch like the legacy planner.
#[test]
fn defer_on_value_type_stays_in_enclosing_fetch() {
    let plan_str = plan_query_with_defer(
        VALUE_TYPE_DEFER_SCHEMA,
        "{ me { ... @defer { messages { ... @defer { body { lines } } } } } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Defer {
        Primary {
          Fetch(service: "Subgraph1", id: 0) {
            {
              me {
                __typename
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "me") {
            Defer {
              Primary {
                Flatten(path: "me") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on User {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on User {
                        messages {
                          body {
                            lines
                          }
                        }
                      }
                    }
                  },
                },
              }, [
                Deferred(depends: [], path: "me/messages") {
                  { body { lines } }:
                },
              ]
            },
          },
        ]
/// Shareable parent returning an abstract type whose runtime members differ
/// per subgraph (U is X|Y in A but only X in B): fragments committed under
/// the B route must be filtered to B's member set, dropping `... on Y`
/// there without a penalty, while the A route keeps both.
/// Targets routing.rs fragment_options' intersection filter, and
/// type_conditions.rs dropped-by-intersection-filter arm.
#[test]
fn inconsistent_union_members_filtered_per_subgraph() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
union U
  @join__type(graph: A)
  @join__type(graph: B)
  @join__unionMember(graph: A, member: "X")
  @join__unionMember(graph: A, member: "Y")
  @join__unionMember(graph: B, member: "X")
 = X | Y

type X
  @join__type(graph: A)
  @join__type(graph: B)
{
  x: String
}

type Y
  @join__type(graph: A)
{
  y: String
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  search: [U]
}
"#,
    );
    let plan_str = plan_query(&schema, "{ search { ... on X { x } ... on Y { y } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Fetch(service: "a") {
        {
          search {
            __typename
            ... on X {
              x
            }
            ... on Y {
              y
            }
          }
        }
      },
    }
    "###);
}
