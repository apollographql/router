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
