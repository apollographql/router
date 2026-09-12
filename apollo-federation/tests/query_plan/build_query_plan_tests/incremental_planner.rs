use apollo_federation::query_plan::query_planner::IncrementalPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

fn incremental_config() -> QueryPlannerConfig {
    incremental_config_with_fuel(100_000)
}

fn incremental_config_with_fuel(fuel: u64) -> QueryPlannerConfig {
    QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            enabled: true,
            beam_width: 4,
            fuel,
            ..Default::default()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Single-subgraph: trivial path, no entity hops
// ---------------------------------------------------------------------------

#[test]
fn inc_single_subgraph_query_produces_valid_plan() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User {
            name: String
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              name
              email
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "a") {
            {
              user {
                name
                email
              }
            }
          },
        }
        "###
    );
}

// ---------------------------------------------------------------------------
// Cross-subgraph: key hops and __typename handling
// ---------------------------------------------------------------------------

#[test]
fn inc_cross_subgraph_key_hop_produces_two_fetches() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              name
              email
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                user {
                  __typename
                  name
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
                    email
                  }
                }
              },
            },
          },
        }
        "###
    );
}

#[test]
fn inc_explicit_sibling_typename_is_preserved() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              __typename
              name
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "a") {
            {
              user {
                __typename
                name
              }
            }
          },
        }
        "###
    );
}

#[test]
fn inc_root_typename_is_left_to_router_execution() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            __typename
            user {
              name
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "a") {
            {
              user {
                name
              }
            }
          },
        }
        "###
    );
}

#[test]
fn inc_root_typename_alone() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            __typename
          }
        "#,
        @r###"
        QueryPlan {}
        "###
    );
}

// ---------------------------------------------------------------------------
// Static and progressive overrides
// ---------------------------------------------------------------------------

#[test]
fn inc_static_override_routes_field_to_overriding_subgraph() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
            nickname: String @override(from: "b")
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              name
              nickname
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "a") {
            {
              user {
                name
                nickname
              }
            }
          },
        }
        "###
    );
}

#[test]
fn inc_progressive_override_routes_to_overrider_when_label_active() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
            nickname: String @override(from: "b", label: "test")
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            nickname: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              name
              nickname
            }
          }
        "#,
        QueryPlanOptions {
            override_conditions: vec!["test".to_string()],
            ..Default::default()
        },
        @r###"
        QueryPlan {
          Fetch(service: "a") {
            {
              user {
                name
                nickname
              }
            }
          },
        }
        "###
    );
}

#[test]
fn inc_progressive_override_routes_to_original_when_label_inactive() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
            nickname: String @override(from: "b", label: "test")
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            nickname: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              name
              nickname
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                user {
                  __typename
                  name
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
                    nickname
                  }
                }
              },
            },
          },
        }
        "###
    );
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

#[test]
fn inc_subscription_produces_subscription_plan_node() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type Subscription {
            onUserCreated: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          subscription {
            onUserCreated {
              id
              name
              email
            }
          }
        "#,
        @r###"
        QueryPlan {
          Subscription {
            Primary: {
              Fetch(service: "a") {
                {
                  onUserCreated {
                    __typename
                    name
                    id
                  }
                }
              },
            },
            Rest: {
              Sequence {
                Flatten(path: "onUserCreated") {
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
            },
          },
        }
        "###
    );
}

// ---------------------------------------------------------------------------
// Mutations: per-field sequential planning
// ---------------------------------------------------------------------------

#[test]
fn inc_mutation_produces_sequential_plan() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type Mutation {
            createUser(name: String!): User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          mutation {
            createUser(name: "Alice") {
              id
              name
              email
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                createUser(name: "Alice") {
                  __typename
                  name
                  id
                }
              }
            },
            Flatten(path: "createUser") {
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
        }
        "###
    );
}

#[test]
fn inc_mutation_multiple_fields_are_sequential() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type Mutation {
            createUser(name: String!): User
            updateUser(id: ID!, name: String!): User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          mutation {
            createUser(name: "Alice") {
              id
              name
            }
            updateUser(id: "1", name: "Bob") {
              id
              name
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                createUser(name: "Alice") {
                  name
                  id
                }
              }
            },
            Fetch(service: "a") {
              {
                updateUser(id: "1", name: "Bob") {
                  name
                  id
                }
              }
            },
          },
        }
        "###
    );
}

// ---------------------------------------------------------------------------
// Greedy tiebreak correction by backtracking
// ---------------------------------------------------------------------------

/// Baseline for the backtracking test below: at fuel 0 the greedy pass
/// keeps its suboptimal tiebreak (3 fetches). The snapshot pins the current
/// tiebreak ranking, not required behavior; a legitimate ranking change may
/// churn it.
#[test]
fn inc_greedy_tiebreak_mistake_survives_without_fuel() {
    let greedy_config = incremental_config_with_fuel(0);
    let planner = planner!(
        config = greedy_config,
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            profile: Profile @shareable
          }

          type Profile @key(fields: "id") {
            id: ID!
          }
        "#,
        c: r#"
          type User @key(fields: "id") {
            id: ID!
            profile: Profile @shareable
          }

          type Profile @key(fields: "id") {
            id: ID!
            detail: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              profile {
                detail
              }
            }
          }
        "#,
        @r###"
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
        "###
    );
}

#[test]
fn inc_greedy_tiebreak_mistake_is_corrected_by_backtracking() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            profile: Profile @shareable
          }

          type Profile @key(fields: "id") {
            id: ID!
          }
        "#,
        c: r#"
          type User @key(fields: "id") {
            id: ID!
            profile: Profile @shareable
          }

          type Profile @key(fields: "id") {
            id: ID!
            detail: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            user {
              profile {
                detail
              }
            }
          }
        "#,
        @r###"
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
        "###
    );
}

// ---------------------------------------------------------------------------
// Error handling: incomplete plans must error, never silently drop fields
// ---------------------------------------------------------------------------

#[test]
fn inc_shareable_parent_hop_reaches_keyless_child_detail() {
    let config = incremental_config_with_fuel(0);
    let planner = planner!(
        config = config,
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            profile: Profile @shareable
          }

          type Profile {
            x: Int @shareable
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            profile: Profile @shareable
          }

          type Profile {
            x: Int @shareable
            detail: String
          }
        "#,
    );
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ user { profile { detail } } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    // Even at fuel 0 the greedy pass finds the complete plan through the
    // shareable parent in b; pin that so the error-path test below stays
    // the only place exercising planning failure.
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("this topology plans completely");
    let plan_str = format!("{plan}");
    assert!(
        plan_str.contains("detail"),
        "plan must fetch the requested field: {plan_str}"
    );
}

/// A genuinely unplannable state: the only subgraph resolving the field is
/// disabled, so the greedy pass drops it, no complete candidate is ever
/// recorded, and planning must fail with the disabled-subgraphs error, not
/// return a partial plan.
#[test]
fn inc_incomplete_plan_is_an_error_not_a_partial_plan() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ user { email } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let err = planner
        .build_query_plan(
            &document,
            None,
            QueryPlanOptions {
                disabled_subgraph_names: std::iter::once("b".to_string()).collect(),
                ..Default::default()
            },
        )
        .expect_err("email is only resolvable in the disabled subgraph");
    assert!(
        err.to_string()
            .contains("No plan was found when subgraphs were disabled"),
        "expected the disabled-subgraphs planning error, got: {err}",
    );
}

// ---------------------------------------------------------------------------
// Cooperative cancellation
// ---------------------------------------------------------------------------

#[test]
fn inc_cooperative_cancellation_stops_planning() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ user { name email } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let cancel = || std::ops::ControlFlow::Break(());
    let result = planner.build_query_plan(
        &document,
        None,
        QueryPlanOptions {
            check_for_cooperative_cancellation: Some(&cancel),
            ..Default::default()
        },
    );
    let err = result.expect_err("cancelled planning should error");
    assert!(
        err.to_string()
            .contains("the caller requested cancellation"),
        "expected the cancellation error specifically (not a generic \
         planning failure), got: {err}",
    );
}

/// When a parent field is routable to multiple subgraphs, the greedy pass
/// (beam=1) picks the best-ranked option — which may lack the needed child
/// field. Because the child type has no @key, entity resolution cannot
/// bridge the gap, so the greedy pass drops the selection. Subsequent BULB
/// iterations (beam > 1) explore the alternative parent routing that
/// reaches the correct subgraph.
#[test]
fn inc_keyless_child_behind_wrong_ranked_hop_recovered_by_search() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
        type Query {
            parent: Parent
        }

        type Parent @key(fields: "id") {
            id: ID!
            child: Child @shareable
        }

        type Child {
            value: Int
        }
        "#,
        b: r#"
        type Parent @key(fields: "id") {
            id: ID!
            child: Child @shareable
        }

        type Child {
            leaf: String
        }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
        {
            parent {
                child {
                    leaf
                }
            }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            parent {
              __typename
              id
            }
          }
        },
        Flatten(path: "parent") {
          Fetch(service: "b") {
            {
              ... on Parent {
                __typename
                id
              }
            } =>
            {
              ... on Parent {
                child {
                  leaf
                }
              }
            }
          },
        },
      },
    }
    "###
    );
}

/// Same topology as above but with fuel=0. Fuel only starts burning once a
/// complete plan exists, so even fuel=0 must keep searching past the
/// incomplete greedy result and find the key-hop alternative.
#[test]
fn inc_fuel_zero_still_finds_complete_plan_for_keyless_child() {
    let planner = planner!(
        config = incremental_config_with_fuel(0),
        a: r#"
        type Query {
            parent: Parent
        }

        type Parent @key(fields: "id") {
            id: ID!
            child: Child @shareable
        }

        type Child {
            value: Int
        }
        "#,
        b: r#"
        type Parent @key(fields: "id") {
            id: ID!
            child: Child @shareable
        }

        type Child {
            leaf: String
        }
        "#,
    );

    let api_schema = planner.api_schema();
    let doc = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        r#"{ parent { child { leaf } } }"#,
        "op.graphql",
    )
    .expect("valid operation");

    let plan = planner
        .build_query_plan(&doc, None, Default::default())
        .expect(
            "fuel=0 must still find a complete plan; fuel bounds optimization, not completeness",
        );
    let plan_str = plan.to_string();
    assert!(
        plan_str.contains("leaf"),
        "plan must fetch the stranded child field: {plan_str}"
    );
    assert!(
        plan_str.contains("b"),
        "plan must route through the key hop to reach `leaf`: {plan_str}"
    );
}

// ---------------------------------------------------------------------------
// Diamond-shaped key dependency
// ---------------------------------------------------------------------------

// D's compound key requires fields split across B and C. Neither subgraph
// alone can satisfy D's key, so the planner fans out to both in parallel
// and converges at D once both halves are available.
//
//       A          @key(fields: "id")
//      / \
//     B   C        B provides `code`, C provides `region`
//      \ /
//       D          @key(fields: "code region"), owns `details`
//
#[test]
fn inc_diamond_shaped_compound_key_dependency() {
    let planner = planner!(
        config = incremental_config(),
        A: r#"
          type Query { t: T }
          type T @key(fields: "id") { id: ID! }
        "#,
        B: r#"
          type T @key(fields: "id") {
            id: ID!
            code: String @shareable
          }
        "#,
        C: r#"
          type T @key(fields: "id") {
            id: ID!
            region: String @shareable
          }
        "#,
        D: r#"
          type T @key(fields: "code region") {
            code: String @shareable
            region: String @shareable
            details: String
          }
        "#
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              details
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "A") {
          {
            t {
              __typename
              id
            }
          }
        },
        Parallel {
          Flatten(path: "t") {
            Fetch(service: "C") {
              {
                ... on T {
                  __typename
                  id
                }
              } =>
              {
                ... on T {
                  region
                }
              }
            },
          },
          Flatten(path: "t") {
            Fetch(service: "B") {
              {
                ... on T {
                  __typename
                  id
                }
              } =>
              {
                ... on T {
                  code
                }
              }
            },
          },
        },
        Flatten(path: "t") {
          Fetch(service: "D") {
            {
              ... on T {
                __typename
                code
                region
              }
            } =>
            {
              ... on T {
                details
              }
            }
          },
        },
      },
    }
    "###
    );
}

// ---------------------------------------------------------------------------
// Defer fallback and statistics
// ---------------------------------------------------------------------------

/// The incremental planner has no defer support yet; a deferred operation
/// must fall back to the legacy planner and keep its DeferNode rather than
/// silently planning every field eagerly.
#[test]
fn inc_defer_falls_back_to_legacy_planner() {
    let mut config = incremental_config();
    config.incremental_delivery.enable_defer = true;
    let planner = planner!(
        config = config,
        a: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        b: r#"
          type T @key(fields: "id") {
            id: ID!
            v1: Int
            v2: Int
          }
        "#,
    );
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        r#"{ t { v1 ... @defer { v2 } } }"#,
        "test.graphql",
    )
    .expect("valid graphql document");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("deferred operation plans via the legacy planner");
    let plan_str = format!("{plan}");
    assert!(
        plan_str.contains("Defer"),
        "deferred operations must keep their DeferNode, got: {plan_str}",
    );
}

/// Mutation planning runs one search per top-level field; the statistics
/// must accumulate across those searches rather than keep only the last.
#[test]
fn inc_mutation_statistics_accumulate_across_fields() {
    let config = incremental_config_with_fuel(0);
    let planner = planner!(
        config = config,
        a: r#"
          type Query {
            q: Int
          }

          type Mutation {
            m1: Int
            m2: Int
          }
        "#,
    );
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "mutation { m1 m2 }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("mutation plans");
    assert_eq!(
        plan.statistics.evaluated_plan_count.get(),
        2,
        "each per-field greedy search evaluates one plan; the counts must sum",
    );
}

// ---------------------------------------------------------------------------
// Root hops and condition hoisting
// ---------------------------------------------------------------------------

#[test]
fn inc_query_field_root_hops_to_other_subgraph() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            nested: Query @shareable
            a: Int
          }
        "#,
        b: r#"
          type Query {
            b: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            nested {
              b
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            nested {
              __typename
            }
          }
        },
        Flatten(path: "nested") {
          Fetch(service: "b") {
            {
              b
            }
          },
        },
      },
    }
    "###
    );
}

#[test]
fn inc_fully_conditioned_fetch_hoists_multiple_variables() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            user: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        b: r#"
          type User @key(fields: "id") {
            id: ID!
            email: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Op($a: Boolean!, $b: Boolean!) {
            user {
              name
              email @include(if: $a) @skip(if: $b)
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "a") {
          {
            user {
              __typename
              name
              id
            }
          }
        },
        Include(if: $a) {
          Skip(if: $b) {
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
        },
      },
    }
    "###
    );
}
