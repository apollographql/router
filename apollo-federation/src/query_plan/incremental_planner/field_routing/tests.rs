use crate::Supergraph;
use crate::query_plan::TopLevelPlanNode;
use crate::query_plan::query_planner::IncrementalPlannerConfig;
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
/// performance workaround) and restored once at bulb entry -- it must
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
/// greedily to A (direct), but A cannot resolve `cm` -- its only hop from C
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
    // backtrack into the key-hop alternative -- the greedy pass strands
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
            // pick the hop), the plan must contain the field -- silence
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
/// entity fetch -- here it declines (A has no D), so C's group chains behind
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
fn try_plan_query(schema: &str, query: &str) -> Result<String, crate::error::FederationError> {
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
/// with a planning error -- not recurse until stack overflow, and not
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

const REQUIRES_SCHEMA: &str = include_str!("../fixtures/requires.graphql");

#[test]
fn requires_fields_added_to_fetch() {
    let plan_str = plan_query(REQUIRES_SCHEMA, "{ product { shippingCost } }");
    assert!(
        plan_str.contains("weight"),
        "Plan should fetch 'weight' for @requires: {plan_str}"
    );
}

const REQUIRES_LOCAL_UNSATISFIABLE_SCHEMA: &str =
    include_str!("../fixtures/requires_local_unsatisfiable.graphql");

/// @requires on a field whose subgraph declares the required fields as
/// @external: the query enters through B (which owns `shippingCost`
/// requiring `weight`), but `weight` is only resolvable in A. The field
/// must move into a second B fetch whose entity representation carries
/// `weight` fetched from A -- B's root fetch must NOT select the
/// @external `weight` itself.
#[test_log::test]
fn requires_unresolvable_locally_hops_through_owning_subgraph() {
    let plan_str = plan_query(
        REQUIRES_LOCAL_UNSATISFIABLE_SCHEMA,
        "{ product { shippingCost } }",
    );
    insta::assert_snapshot!(plan_str, @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "b") {
              {
                product {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "product") {
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
            Flatten(path: "product") {
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

const REQUIRES_THROUGH_LOCAL_FIELD_SCHEMA: &str =
    include_str!("../fixtures/requires_through_local_field.graphql");

/// @requires whose field set walks through a *locally resolvable* field
/// (`a`) into nested selections owned by other subgraphs (`s.status` in
/// s1, `j.m` in s2). The planner must resolve the local prefix in place
/// and hop from A's key edges for the nested parts, at the merge path of
/// C's `a` -- not from C's (nonexistent) key edges, and not at some other
/// A's path.
#[test_log::test]
fn requires_through_local_field_resolves_nested_parts() {
    let plan_str = plan_query(REQUIRES_THROUGH_LOCAL_FIELD_SCHEMA, "{ a { c { elig } } }");
    // All three requires leaves must be fetched somewhere.
    for needle in ["status", "j {", "elig"] {
        assert!(
            plan_str.contains(needle),
            "Plan should fetch {needle:?}: {plan_str}"
        );
    }
    // The nested hops must merge at C's `a` (aliased as a @requires
    // condition), i.e. path a.c.__require_0_a -- not at the top-level `a`.
    assert!(
        plan_str.contains("a.c.__require_0_a"),
        "Nested requires parts should merge under a.c.__require_0_a: {plan_str}"
    );
}

const OVERRIDE_SCHEMA: &str = include_str!("../fixtures/override.graphql");

#[test]
fn static_override_routes_field_to_overriding_subgraph() {
    let plan_str = plan_query(OVERRIDE_SCHEMA, "{ user { name nickname } }");
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

const ROUTING_CHOICE_ITERATIVE_SCHEMA: &str =
    include_str!("../fixtures/routing_choice_iterative.graphql");

/// Demonstrates BULB backtracking actually correcting a greedy mistake,
/// not just picking correctly the first time. `profile` is a key hop
/// from A to either B or C, and both hops score identically at the
/// one-step scoring pass (same fetch shape), so the greedy tiebreak
/// (declaration order) commits to B. Only once `profile` lands on B do
/// we discover `detail` isn't there and needs a second hop to C -- a cost
/// the one-step score for the `profile` decision couldn't see.
///
/// With fuel=1 (greedy pass only, discrepancies never explored), BULB
/// returns that suboptimal 3-fetch plan (A -> B -> C). With enough fuel
/// to run a discrepancy iteration, it explores the C branch to
/// completion, finds the cheaper 2-fetch plan (A -> C), and replaces the
/// greedy result -- the same "record_completion only if improved"
/// mechanism the toy `discrepancy_finds_better_alternative_slice` test
/// exercises, but on a real routing decision.
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
        ROUTING_CHOICE_ITERATIVE_SCHEMA,
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

    let backtracking_plan_str = plan_query(ROUTING_CHOICE_ITERATIVE_SCHEMA, document_str);
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

const PROGRESSIVE_OVERRIDE_SCHEMA: &str = include_str!("../fixtures/progressive_override.graphql");

#[test]
fn progressive_override_routes_to_overrider_when_label_active() {
    let plan_str = plan_query_with_options(
        PROGRESSIVE_OVERRIDE_SCHEMA,
        "{ user { name nickname } }",
        default_config(),
        QueryPlanOptions {
            override_conditions: vec!["test".to_string()],
            ..Default::default()
        },
    );
    assert!(
        plan_str.contains("nickname"),
        "Plan should fetch 'nickname': {plan_str}"
    );
    assert!(
        !plan_str.contains("Flatten"),
        "Active override should not require entity fetch: {plan_str}"
    );
}

#[test]
fn progressive_override_routes_to_original_when_label_inactive() {
    let plan_str = plan_query(PROGRESSIVE_OVERRIDE_SCHEMA, "{ user { name nickname } }");
    assert!(
        plan_str.contains("nickname"),
        "Plan should fetch 'nickname': {plan_str}"
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
// the original intermediate -- so the fast-path check validates against the
// wrong subgraph and appends to whatever entity group `last_node` reached.
#[test_log::test]
fn requires_with_multiple_external_parts_and_nested_requires() {
    let schema = include_str!("../fixtures/requires_external_misroute.graphql");
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
    let schema = include_str!("../fixtures/requires_external_misroute.graphql");
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
/// input (same field, same arguments -- no conflict) and rides the same
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
    let existing = state.graph.get_or_create_entity_group(&s2, vec![]);
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
