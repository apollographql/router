use apollo_federation::query_plan::query_planner::IncrementalPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

fn incremental_config() -> QueryPlannerConfig {
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

#[test]
fn inc_greedy_tiebreak_mistake_is_corrected_by_backtracking_greedy() {
    let greedy_config = QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            fuel: 0,
            ..incremental_config().incremental_planner
        },
        ..incremental_config()
    };
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
fn inc_incomplete_plan_is_an_error_not_a_partial_plan() {
    let config = QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            fuel: 0,
            ..incremental_config().incremental_planner
        },
        ..incremental_config()
    };
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
    let result = planner.build_query_plan(&document, None, Default::default());
    match result {
        Err(_) => {}
        Ok(plan) => {
            let plan_str = format!("{plan}");
            assert!(
                plan_str.contains("detail"),
                "Planner returned an incomplete plan instead of erroring: {plan_str}"
            );
        }
    }
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
    assert!(
        result.is_err(),
        "Cancelled planning should error, got:\n{}",
        result.map(|p| p.to_string()).unwrap_or_default(),
    );
}

/// When a parent field is routable to multiple subgraphs, the greedy pass
/// (beam=1) picks the best-ranked option — which may lack the needed child
/// field. Because the child type has no @key, entity resolution cannot
/// bridge the gap, so the greedy pass drops the selection. Subsequent BULB
/// iterations (beam > 1) explore the alternative parent routing that
/// reaches the correct subgraph.
#[test]
fn inc_fuel_needed_for_keyless_child_behind_wrong_ranked_hop() {
    let planner = planner!(
        config = incremental_config(),
        SubgraphA: r#"
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
        SubgraphB: r#"
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
        Fetch(service: "SubgraphA") {
          {
            parent {
              __typename
              id
            }
          }
        },
        Flatten(path: "parent") {
          Fetch(service: "SubgraphB") {
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
        config = {
            QueryPlannerConfig {
                incremental_planner: IncrementalPlannerConfig {
                    enabled: true,
                    fuel: 0,
                    ..Default::default()
                },
                ..Default::default()
            }
        },
        SubgraphA: r#"
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
        SubgraphB: r#"
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
        plan_str.contains("SubgraphB"),
        "plan must route through the key hop to reach `leaf`: {plan_str}"
    );
}
