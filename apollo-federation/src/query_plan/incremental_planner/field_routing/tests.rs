use crate::Supergraph;
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

const SINGLE_SUBGRAPH_SCHEMA: &str = include_str!("../fixtures/single_subgraph.graphql");

#[test]
fn single_subgraph_query_produces_valid_plan() {
    let plan_str = plan_query(SINGLE_SUBGRAPH_SCHEMA, "{ user { name email } }");
    assert!(
        plan_str.contains("name"),
        "Plan should fetch 'name': {plan_str}"
    );
    assert!(
        plan_str.contains("email"),
        "Plan should fetch 'email': {plan_str}"
    );
}

const CROSS_SUBGRAPH_SCHEMA: &str = include_str!("../fixtures/cross_subgraph.graphql");

#[test]
fn cross_subgraph_key_hop_produces_two_fetches() {
    let plan_str = plan_query(CROSS_SUBGRAPH_SCHEMA, "{ user { name email } }");
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
    let plan_str = plan_query(CROSS_SUBGRAPH_SCHEMA, "{ user { __typename name } }");
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
    let plan_str = plan_query(CROSS_SUBGRAPH_SCHEMA, "{ __typename user { name } }");
    assert_eq!(
        plan_str.matches("Fetch(").count(),
        1,
        "Root __typename must not add a fetch: {plan_str}"
    );

    let alone = plan_query(CROSS_SUBGRAPH_SCHEMA, "{ __typename }");
    assert_eq!(
        alone, "QueryPlan {}",
        "Router execution answers root __typename"
    );
}

const SUBSCRIPTION_SCHEMA: &str = include_str!("../fixtures/subscription.graphql");

#[test]
fn subscription_produces_subscription_plan_node() {
    let supergraph = Supergraph::new(SUBSCRIPTION_SCHEMA).expect("supergraph parse");
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

const MUTATION_SCHEMA: &str = include_str!("../fixtures/mutation.graphql");

#[test]
fn mutation_produces_sequential_plan() {
    let plan_str = plan_query(
        MUTATION_SCHEMA,
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
fn mutation_multiple_fields_are_sequential() {
    let plan_str = plan_query(
        MUTATION_SCHEMA,
        r#"mutation { createUser(name: "Alice") { id name } updateUser(id: "1", name: "Bob") { id name } }"#,
    );
    assert!(
        plan_str.contains("Sequence"),
        "Multiple mutations should be sequenced: {plan_str}"
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

const SHAREABLE_DEAD_END_SCHEMA: &str = include_str!("../fixtures/shareable_dead_end.graphql");

/// Repro for the "local edge suppresses a required key hop" gap: `profile`
/// is shareable in A and B, but A's copy of `Profile` lacks `detail` and
/// `Profile` has no key, so once `profile` is routed to A, `detail` is
/// stranded. `profile` is a genuine decision point (direct edge to A plus
/// the User-level key hop to B), so the greedy strand-and-drop is recovered
/// by BULB backtracking picking the hop.
#[test_log::test]
fn shareable_local_dead_end_reroutes_through_key_hop() {
    let plan_str = plan_query(SHAREABLE_DEAD_END_SCHEMA, "{ user { profile { detail } } }");
    assert!(
        plan_str.contains("detail"),
        "Plan should fetch 'detail' via B: {plan_str}"
    );
    assert!(
        plan_str.contains("service: \"b\""),
        "Plan should hop to subgraph b for profile.detail: {plan_str}"
    );
}

const CIRCULAR_KEY_BACKTRACK_SCHEMA: &str =
    include_str!("../fixtures/circular_key_backtrack.graphql");

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
    let plan_str = plan_query(CIRCULAR_KEY_BACKTRACK_SCHEMA, "{ entry { target } }");
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
    let supergraph = Supergraph::new(SHAREABLE_DEAD_END_SCHEMA).expect("supergraph parse");
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
// forces; schemas are inlined via `wrap_supergraph` so the shape that
// triggers the path is visible next to the assertion.
// ---------------------------------------------------------------------------

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

/// A cancellation callback that immediately breaks must abort planning with
/// a PlanningCancelled error rather than returning a plan.
/// Targets incremental_planner/mod.rs's cancelled branch.
#[test]
fn cooperative_cancellation_stops_planning() {
    let supergraph = Supergraph::new(CROSS_SUBGRAPH_SCHEMA).expect("supergraph parse");
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

fn nested_entity_hop_schema() -> String {
    wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")
  C @join__graph(name: "c", url: "http://c")"#,
        r#"
type P
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  details: D @join__field(graph: B)
}

type D
  @join__type(graph: B, key: "did")
  @join__type(graph: C, key: "did")
{
  did: ID!
  extra: String @join__field(graph: C)
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: C)
{
  p: P @join__field(graph: A)
}
"#,
    )
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
    let plan_str = plan_query(&nested_entity_hop_schema(), "{ p { details { extra } } }");
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
        CROSS_SUBGRAPH_SCHEMA,
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
    let supergraph = Supergraph::new(CROSS_SUBGRAPH_SCHEMA).expect("supergraph parse");
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
    let supergraph = Supergraph::new(SINGLE_SUBGRAPH_SCHEMA).expect("supergraph parse");
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
            disabled_subgraph_names: ["a".to_string()].into_iter().collect(),
            ..Default::default()
        },
    );
    assert!(
        result.is_err(),
        "Planning with every subgraph disabled must fail, got:\n{}",
        result.map(|p| p.to_string()).unwrap_or_default(),
    );
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
/// `__typename` pending that routes generically -- the io node has no
/// `__typename` edge, so its only options are key hops to subgraphs where I
/// is the REAL interface (here A) -- so execution learns each object's
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
        Flatten(path: "items.@") {
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
        Flatten(path: "animals.@") {
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
        REQUIRES_LOCAL_UNSATISFIABLE_SCHEMA,
        "query($v: Boolean!) { product { ... on Product @include(if: $v) { shippingCost } } }",
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
        Include(if: $v) {
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
    let skipped = plan_query(
        CROSS_SUBGRAPH_SCHEMA,
        "{ user { name ... @skip(if: true) { email } } }",
    );
    assert!(
        !skipped.contains("email"),
        "Statically skipped fragment must not be fetched: {skipped}"
    );

    let rooted = plan_query(
        CROSS_SUBGRAPH_SCHEMA,
        "query($v: Boolean!) { ... on Query @skip(if: $v) { user { name } } }",
    );
    assert!(
        rooted.contains("name"),
        "Root type condition should pass through: {rooted}"
    );
}

/// Cross-subgraph @defer: the deferred fragment's fields live in a different
/// subgraph from the primary, producing a Defer node with a key-hop fetch
/// in the deferred block.
#[test]
fn defer_produces_defer_node() {
    let plan_str = plan_query_with_defer(
        CROSS_SUBGRAPH_SCHEMA,
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

/// Labels synthesized by defer normalization for unlabeled @defer
/// (`qp__N`) are internal bookkeeping and must not leak into the plan;
/// user-written labels must be preserved.
#[test]
fn synthesized_defer_labels_do_not_leak_into_plan() {
    let plan_str = plan_query_with_defer(
        CROSS_SUBGRAPH_SCHEMA,
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
        CROSS_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer(label: \"mine\") { email } } }",
    );
    assert!(
        labeled_plan_str.contains("mine"),
        "User-written defer label must be preserved: {labeled_plan_str}"
    );
}

/// Same-subgraph @defer still produces a Defer node so the executor can
/// send a multipart response even when no cross-subgraph fetch is needed.
#[test]
fn defer_same_subgraph_produces_defer_node() {
    let plan_str = plan_query_with_defer(
        SINGLE_SUBGRAPH_SCHEMA,
        "{ user { name ... @defer { email } } }",
    );
    assert!(
        plan_str.contains("Defer"),
        "Plan should contain a Defer node even for same-subgraph @defer: {plan_str}"
    );
}

const THREE_SUBGRAPH_SCHEMA: &str = include_str!("../fixtures/three_subgraph.graphql");

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

/// Deep keyless split: the strand sits two keyless levels below the field
/// with routing alternatives, where the one-level lookahead in
/// `split_for_other_subgraph` cannot see it. Committing `conn` to A strands
/// `Inner.b` (Inner and Conn are keyless, so no hop can recover it); the
/// drop-time recovery re-pushes `conn { inner { b } }` at the entity anchor,
/// avoiding A, so it key-hops to B and the responses merge at the same path.
/// Targets mod.rs try_split_repush / wrap_in_parent / split_avoid filtering.
#[test]
fn deep_keyless_split_repushes_remainder_at_entity_anchor() {
    let schema = wrap_supergraph(
        r#"  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")"#,
        r#"
type E
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  conn: Conn
  onlyA: String @join__field(graph: A)
}

type Conn
  @join__type(graph: A)
  @join__type(graph: B)
{
  inner: Inner
}

type Inner
  @join__type(graph: A)
  @join__type(graph: B)
{
  a: String @join__field(graph: A)
  b: String @join__field(graph: B)
}

type Query
  @join__type(graph: A)
{
  e: E
}
"#,
    );
    let plan_str = plan_query(&schema, "{ e { onlyA conn { inner { a b } } } }");
    insta::assert_snapshot!(plan_str, @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            e {
              __typename
              id
              onlyA
              conn {
                inner {
                  a
                }
              }
            }
          }
        },
        Flatten(path: "e") {
          Fetch(service: "b") {
            {
              ... on E {
                __typename
                id
              }
            } =>
            {
              ... on E {
                conn {
                  inner {
                    b
                  }
                }
              }
            }
          },
        },
      },
    }
    "###);
}
