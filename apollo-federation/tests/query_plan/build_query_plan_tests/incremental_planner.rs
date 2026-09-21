use apollo_compiler::Name;
use apollo_federation::query_plan::FetchDataPathElement;
use apollo_federation::query_plan::FetchDataRewrite;
use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::query_plan::query_planner::IncrementalPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
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

fn incremental_defer_config() -> QueryPlannerConfig {
    QueryPlannerConfig {
        incremental_planner: IncrementalPlannerConfig {
            enabled: true,
            beam_width: 4,
            fuel: 100_000,
            ..Default::default()
        },
        incremental_delivery: QueryPlanIncrementalDeliveryConfig { enable_defer: true },
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
// Cross-subgraph: key hops, __typename, conditions, errors, cancellation
//
// All scenarios below share the same two-subgraph schema where User lives
// in both `a` (owns name) and `b` (owns email), joined by @key(fields: "id").
// ---------------------------------------------------------------------------

#[test]
fn inc_two_subgraph_key_hop() {
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

    // Querying fields split across subgraphs produces a key-hop fetch chain.
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

    // An explicit __typename sibling is preserved in the fetch selection.
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

    // Root-level __typename is left to router execution, not fetched.
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

    // A bare root __typename produces an empty plan.
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

    // Conditioned fields hoist their @include/@skip wrappers around the
    // dependent fetch, keeping the unconditioned root fetch unconditional.
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

    // Disabling the only subgraph that can resolve a field must produce a
    // planning error, not a partial plan that silently drops the field.
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

    // Cooperative cancellation: a cancel callback that fires immediately
    // must cause planning to fail with the cancellation error.
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
fn inc_progressive_override_routing() {
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

    // When the override label is active, the overrider resolves the field.
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

    // When the override label is inactive, the original subgraph keeps the field.
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

/// Top-level mutation fields each get their own fetch, even when they
/// resolve in the same subgraph. Unlike the legacy planner, which
/// coalesces contiguous same-subgraph root fields into one fetch, this
/// planner intentionally keeps one fetch per field: each field is its own
/// search, and per-field fetches keep serial execution boundaries explicit.
#[test]
fn inc_mutation_multiple_fields_are_not_merged() {
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

/// The one-fetch-per-field shape is order-preserving by construction:
/// same-subgraph fields interleaved with another subgraph's field stay
/// three separate fetches in document order.
#[test]
fn inc_mutation_interleaved_subgraph_fields_stay_in_document_order() {
    let planner = planner!(
        config = incremental_config(),
        a: r#"
          type Query {
            x: Int
          }

          type Mutation {
            m1: Int
            m2: Int
          }
        "#,
        b: r#"
          type Query {
            y: Int
          }

          type Mutation {
            m3: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          mutation {
            m1
            m3
            m2
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "a") {
              {
                m1
              }
            },
            Fetch(service: "b") {
              {
                m3
              }
            },
            Fetch(service: "a") {
              {
                m2
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

// ---------------------------------------------------------------------------
// Keyless child behind wrong-ranked hop
// ---------------------------------------------------------------------------

/// When a parent field is routable to multiple subgraphs, the greedy pass
/// (beam=1) picks the best-ranked option, which may lack the needed child
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
// Defer across subgraphs and statistics
// ---------------------------------------------------------------------------

/// Cross-subgraph @defer: the deferred field's entity fetch lands in the
/// Deferred block, and the deferred selection stays out of the primary.
#[test]
fn inc_defer_cross_subgraph_produces_deferred_entity_fetch() {
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
    assert_plan!(
        &planner,
        r#"{ t { v1 ... @defer { v2 } } }"#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { v1 } }:
          Sequence {
            Fetch(service: "a", id: 0) {
              {
                t {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "b") {
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
        }, [
          Deferred(depends: [0], path: "t") {
            { v2 }:
            Flatten(path: "t") {
              Fetch(service: "b") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v2
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
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
// Root hops
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
            b2: Int
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

    // Sibling fields resolved through the same root hop share one fetch
    // group instead of producing one fetch each.
    assert_plan!(
        &planner,
        r#"
          {
            nested {
              b
              b2
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
              b2
            }
          },
        },
      },
    }
    "###
    );
}

// ---------------------------------------------------------------------------
// Circular keys
// ---------------------------------------------------------------------------

/// A forced condition commit whose greedy choice strands a descendant on a
/// circular key must backtrack to the ancestor's alternative. `target` lives
/// only in T, keyed on `c { cid cm }`. Routing that key: `c` commits
/// greedily to A (direct), but A cannot resolve `cm`. Its only hop from C
/// is T's circular `{cid cm}` key, so the commit fails. The condition `c`
/// was forced (never a BULB decision), so recovery must come from the
/// fast-forward trail: rewind `c` to its key hop into B, where the whole
/// key resolves.
#[test]
fn inc_circular_key_backtracks_to_alternative() {
    // Pre-composed with join/v0.2 because join/v0.5 composition omits
    // per-field @join__field annotations on fields present in every subgraph,
    // and the query graph builder then fails to rebase key conditions that
    // reference fields absent from a source subgraph. The circular key
    // pattern (E's key in T requires `c { cid cm }`, but A's C has no `cm`)
    // triggers this rebase gap before the incremental planner's circular-key
    // detection can kick in.
    let supergraph_sdl = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
{
  query: Query
}

directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")
  T @join__graph(name: "t", url: "http://t")
}

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query
  @join__type(graph: A)
{
  entry: E @join__field(graph: A)
}

type E
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: T, key: "c { cid cm }")
{
  id: ID! @join__field(graph: A) @join__field(graph: B)
  c: C @join__field(graph: A) @join__field(graph: B) @join__field(graph: T)
  target: String @join__field(graph: T)
}

type C
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: T, key: "cid cm")
{
  cid: ID! @join__field(graph: A) @join__field(graph: B) @join__field(graph: T)
  cm: String @join__field(graph: B) @join__field(graph: T)
}
"#;
    let supergraph = apollo_federation::Supergraph::new(supergraph_sdl).expect("valid supergraph");
    let planner = apollo_federation::query_plan::query_planner::QueryPlanner::new(
        &supergraph,
        incremental_config(),
    )
    .expect("can create query planner");
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ entry { target } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let result = planner.build_query_plan(&document, None, Default::default());
    let plan_str = result
        .as_ref()
        .map(|p| p.to_string())
        .unwrap_or_else(|e| format!("<error: {e}>"));
    assert!(
        result.is_ok(),
        "Planning should succeed for circular key schema: {plan_str}"
    );
    assert!(
        plan_str.contains("target"),
        "Plan should fetch 'target' from T: {plan_str}"
    );
    // The key's `c` subtree must route through B (where `cm` resolves),
    // not A (where `cm` is missing and the key is circular).
    assert!(
        plan_str.contains("service: \"b\""),
        "Plan should route the key's `c` subtree through subgraph b: {plan_str}"
    );
    assert!(
        plan_str.contains("cm"),
        "Plan should fetch the key field 'cm': {plan_str}"
    );
}

/// A field reachable only through two key hops (A has `id`, B has `id` and
/// `bid`, C has `bid` and the field). No single hop from A reaches `target`
/// because A lacks `bid`, so the planner must chain A->B->C.
#[test]
fn inc_multi_hop_key_chain_reaches_transitive_subgraph() {
    let supergraph_sdl = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
{
  query: Query
}

directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")
  C @join__graph(name: "c", url: "http://c")
}

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query
  @join__type(graph: A)
{
  entry: T @join__field(graph: A)
}

type T
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: B, key: "bid")
  @join__type(graph: C, key: "bid")
{
  id: ID! @join__field(graph: A) @join__field(graph: B)
  bid: ID! @join__field(graph: B) @join__field(graph: C)
  name: String @join__field(graph: A)
  target: String @join__field(graph: C)
}
"#;
    let supergraph = apollo_federation::Supergraph::new(supergraph_sdl).expect("valid supergraph");
    let planner = apollo_federation::query_plan::query_planner::QueryPlanner::new(
        &supergraph,
        incremental_config(),
    )
    .expect("can create query planner");
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ entry { target } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let result = planner.build_query_plan(&document, None, Default::default());
    let plan_str = result
        .as_ref()
        .map(|p| p.to_string())
        .unwrap_or_else(|e| format!("<error: {e}>"));
    assert!(
        result.is_ok(),
        "Planning should succeed for multi-hop chain schema: {plan_str}"
    );
    assert!(
        plan_str.contains("target"),
        "Plan should fetch 'target': {plan_str}"
    );
    // The chain must transit through B to reach C.
    assert!(
        plan_str.contains("service: \"b\""),
        "Plan should include an intermediate fetch from subgraph b: {plan_str}"
    );
    assert!(
        plan_str.contains("service: \"c\""),
        "Plan should include a final fetch from subgraph c: {plan_str}"
    );
}

/// When the only key hop to a target has statically circular conditions
/// and no chain alternative exists, the planner must error rather than
/// silently dropping the field. Here `target` lives only in T, keyed on
/// `c { cid cm }`, but `cm` exists only in T (the same subgraph). No
/// intermediate subgraph (like B in the backtrack test) can resolve `cm`,
/// so no chain or fallback is available.
#[test]
fn inc_unresolvable_circular_key_errors() {
    let supergraph_sdl = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
{
  query: Query
}

directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "a", url: "http://a")
  T @join__graph(name: "t", url: "http://t")
}

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query
  @join__type(graph: A)
{
  entry: E @join__field(graph: A)
}

type E
  @join__type(graph: A, key: "id")
  @join__type(graph: T, key: "c { cid cm }")
{
  id: ID! @join__field(graph: A)
  c: C @join__field(graph: A) @join__field(graph: T)
  target: String @join__field(graph: T)
}

type C
  @join__type(graph: A)
  @join__type(graph: T, key: "cid cm")
{
  cid: ID! @join__field(graph: A) @join__field(graph: T)
  cm: String @join__field(graph: T)
}
"#;
    let supergraph = apollo_federation::Supergraph::new(supergraph_sdl).expect("valid supergraph");
    let planner = apollo_federation::query_plan::query_planner::QueryPlanner::new(
        &supergraph,
        incremental_config(),
    )
    .expect("can create query planner");
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ entry { target } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let result = planner.build_query_plan(&document, None, Default::default());
    assert!(
        result.is_err(),
        "Unresolvable circular key should fail planning, got:\n{}",
        result.as_ref().map(|p| p.to_string()).unwrap_or_default(),
    );
}

// ---------------------------------------------------------------------------
// @requires: condition aliasing and cross-subgraph routing
// ---------------------------------------------------------------------------

// @requires chains alias required fields as __require_N_* in generated
// operations and rename them back with input KeyRenamer rewrites.
// Based on: requires.rs::it_handles_simple_require_chain
#[test]
fn inc_requires_chain_aliases_conditions() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v: Int!
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v: Int! @external
            inner: Int! @requires(fields: "v")
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id") {
            id: ID!
            inner: Int! @external
            outer: Int! @requires(fields: "inner")
          }
        "#
    );
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"
          {
            t {
              outer
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              __typename
              id
              v
            }
          }
        },
        Flatten(path: "t") {
          Fetch(service: "Subgraph2") {
            {
              ... on T {
                __typename
                id
                v
              }
            } =>
            {
              ... on T {
                __require_0_inner: inner
              }
            }
          },
        },
        Flatten(path: "t") {
          Fetch(service: "Subgraph3") {
            {
              ... on T {
                __typename
                id
                __require_0_inner: inner
              }
            } =>
            {
              ... on T {
                outer
              }
            }
          },
        },
      },
    }
    "###
    );
}

/// Two fetches land in SubgraphA: the operation root, and a root hop under
/// `computed`. The hop transitively depends on the root fetch through the
/// @requires condition resolved in SubgraphB, so the two SubgraphA fetches
/// can never merge or share a node despite hitting the same subgraph root.
/// Guards root group and root hop reuse against creating a cycle here.
#[test]
fn inc_root_hop_after_requires_back_into_same_subgraph_stays_split() {
    let planner = planner!(
        config = incremental_config(),
        SubgraphA: r#"
          type Query {
            e: E
            a: Int
          }

          type E @key(fields: "id") {
            id: ID!
            data: Int
          }
        "#,
        SubgraphB: r#"
          type Query {
            b: Int
          }

          type E @key(fields: "id") {
            id: ID!
            data: Int @external
            computed: Query @requires(fields: "data")
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            a
            e {
              computed {
                a
                b
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "SubgraphA") {
          {
            a
            e {
              __typename
              id
              data
            }
          }
        },
        Flatten(path: "e") {
          Fetch(service: "SubgraphB") {
            {
              ... on E {
                __typename
                id
                data
              }
            } =>
            {
              ... on E {
                computed {
                  __typename
                  b
                }
              }
            }
          },
        },
        Flatten(path: "e.computed") {
          Fetch(service: "SubgraphA") {
            {
              a
            }
          },
        },
      },
    }
    "###
    );
}

/// A @requires chain that must route its condition field through a key
/// hop into another subgraph before the dependent field can be fetched.
#[test]
fn inc_requires_routes_condition_via_key_hop() {
    let planner = planner!(
        config = incremental_config(),
        SubgraphA: r#"
        type Query {
            product: Product
        }

        type Product @key(fields: "id") {
            id: ID!
        }
        "#,
        SubgraphB: r#"
        type Product @key(fields: "id") {
            id: ID!
            weight: Float
        }
        "#,
        SubgraphC: r#"
        type Product @key(fields: "id") {
            id: ID!
            weight: Float @external
            shippingEstimate: Float @requires(fields: "weight")
        }
        "#,
    );
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"
        {
            product {
                shippingEstimate
            }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "SubgraphA") {
          {
            product {
              __typename
              id
            }
          }
        },
        Flatten(path: "product") {
          Fetch(service: "SubgraphB") {
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
          Fetch(service: "SubgraphC") {
            {
              ... on Product {
                __typename
                id
                __require_0_weight: weight
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
    "###
    );
}

// The user requests a parameterized field with one set of arguments while a
// sibling's @requires needs the same field with different arguments. The
// condition copy must carry a __require_N_ alias so it doesn't collide with
// the user's selection. Without the alias the planner merges both into one
// fetch and produces invalid GraphQL ("conflicting field arguments").
// Reproduces a customer-reported planning failure.
#[test]
fn inc_user_field_argument_conflict_with_requires_condition() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
            p(arg: Int): Int
        }
        "#,
        Subgraph2: r#"
        type T @key(fields: "id") {
            id: ID!
            p(arg: Int): Int @external
            x: Int @requires(fields: "p(arg: 1)")
        }
        "#,
    );
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"
        {
            t {
                p(arg: 2)
                x
            }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              __typename
              p(arg: 2)
              id
              __require_0_p: p(arg: 1)
            }
          }
        },
        Flatten(path: "t") {
          Fetch(service: "Subgraph2") {
            {
              ... on T {
                __typename
                id
                __require_0_p: p
              }
            } =>
            {
              ... on T {
                x
              }
            }
          },
        },
      },
    }
    "###
    );
}

/// Two @requires on the same entity type need different arguments on a
/// shared condition field `f`. The planner detects the argument conflict,
/// aliases both condition invocations, and splits into separate entity
/// fetches so each gets its own input.
#[test]
fn inc_requires_conflicting_arguments_splits_group() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            f(arg: Int!): Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            f(arg: Int!): Int @external
            a: Int! @requires(fields: "f(arg: 1)")
            b: Int! @requires(fields: "f(arg: 2)")
          }
        "#
    );
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"
          {
            t {
              a
              b
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              __typename
              id
              __require_0_f: f(arg: 1)
              __require_1_f: f(arg: 2)
            }
          }
        },
        Parallel {
          Flatten(path: "t") {
            Fetch(service: "Subgraph2") {
              {
                ... on T {
                  __typename
                  id
                  __require_1_f: f
                }
              } =>
              {
                ... on T {
                  b
                }
              }
            },
          },
          Flatten(path: "t") {
            Fetch(service: "Subgraph2") {
              {
                ... on T {
                  __typename
                  id
                  __require_0_f: f
                }
              } =>
              {
                ... on T {
                  a
                }
              }
            },
          },
        },
      },
    }
    "###
    );
}

/// A field satisfied by an ancestor's @provides is preferred over hopping
/// to another subgraph for the same field.
#[test]
fn inc_provides_prefers_local_resolution() {
    let planner = planner!(
        config = incremental_config(),
        SubgraphA: r#"
        type Query {
            product: Product
        }

        type Product @key(fields: "id") {
            id: ID!
            details: Details @provides(fields: "price")
        }

        type Details @key(fields: "id") {
            id: ID!
            price: Float @external
        }
        "#,
        SubgraphB: r#"
        type Details @key(fields: "id") {
            id: ID!
            price: Float @shareable
            description: String
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
            product {
                details {
                    price
                    description
                }
            }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "SubgraphA") {
          {
            product {
              details {
                __typename
                id
                price
              }
            }
          }
        },
        Flatten(path: "product.details") {
          Fetch(service: "SubgraphB") {
            {
              ... on Details {
                __typename
                id
              }
            } =>
            {
              ... on Details {
                description
              }
            }
          },
        },
      },
    }
    "###
    );
}

/// @interfaceObject fake downcast: the io subgraph has no `__typename` edge
/// for the concrete type, so `push_interface_object_typename` pushes a
/// best-effort `__typename` pending that routes via key hop to the subgraph
/// owning the real interface. The concrete type condition is dropped from
/// the io subgraph's operation.
#[test]
fn inc_interface_object_fake_downcast_fetches_typename() {
    let planner = planner!(
        config = incremental_config(),
        SubgraphA: r#"
        type Query {
            items: [I]
        }

        interface I @key(fields: "id") {
            id: ID!
            name: String
            desc: String
        }

        type X implements I @key(fields: "id") {
            id: ID!
            name: String
            desc: String @external
        }
        "#,
        SubgraphB: r#"
        type Query {
            stuff: [I]
        }

        type I @key(fields: "id") @interfaceObject {
            id: ID!
            desc: String
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
            stuff {
                ... on X {
                    desc
                }
            }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "SubgraphB") {
          {
            stuff {
              __typename
              desc
              id
            }
          }
        },
        Flatten(path: "stuff.@") {
          Fetch(service: "SubgraphA") {
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
    "###
    );
}

/// @interfaceObject fake downcast where the best-effort `__typename` pushed by
/// `push_interface_object_typename` cannot route: the subgraph owning the real
/// interface has a non-resolvable key, so no key hop reaches it from the
/// @interfaceObject subgraph. The planner silently drops the `__typename`
/// (exercising the `best_effort` branch in `drop_unresolvable` and the
/// `recover_doomed` short-circuit) without failing the overall plan.
///
/// Hand-crafted supergraph SDL because `rover supergraph compose` rejects
/// schemas where no subgraph can resolve implementation types of an
/// @interfaceObject interface (SATISFIABILITY_ERROR), yet the incremental
/// planner must handle the topology gracefully at runtime.
#[test]
fn inc_interface_object_best_effort_typename_dropped() {
    let supergraph_sdl = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
{
  query: Query
}

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

interface I
  @join__type(graph: SUBGRAPHA, key: "id", resolvable: false)
  @join__type(graph: SUBGRAPHB, key: "id", isInterfaceObject: true)
{
  id: ID!
  data: String @join__field(graph: SUBGRAPHB)
}

input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}

scalar join__DirectiveArguments
scalar join__FieldSet
scalar join__FieldValue

enum join__Graph {
  SUBGRAPHA @join__graph(name: "SubgraphA", url: "none")
  SUBGRAPHB @join__graph(name: "SubgraphB", url: "none")
}

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query
  @join__type(graph: SUBGRAPHB)
{
  stuff: [I] @join__field(graph: SUBGRAPHB)
}

type X implements I
  @join__implements(graph: SUBGRAPHA, interface: "I")
  @join__type(graph: SUBGRAPHA, key: "id", resolvable: false)
{
  id: ID!
  data: String @join__field(graph: SUBGRAPHA, external: true)
}

type Y implements I
  @join__implements(graph: SUBGRAPHA, interface: "I")
  @join__type(graph: SUBGRAPHA, key: "id", resolvable: false)
{
  id: ID!
  data: String @join__field(graph: SUBGRAPHA, external: true)
}
"#;
    let supergraph = apollo_federation::Supergraph::new(supergraph_sdl).expect("valid supergraph");
    let planner = apollo_federation::query_plan::query_planner::QueryPlanner::new(
        &supergraph,
        incremental_config(),
    )
    .expect("can create query planner");
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ stuff { ... on X { data } } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("planning should succeed even when best-effort __typename cannot route");
    let plan_str = plan.to_string();
    // Single fetch to SubgraphB; the best-effort __typename was silently
    // dropped, so there is no key hop to SubgraphA.
    assert!(
        !plan_str.contains("SubgraphA"),
        "Plan must not key-hop to SubgraphA (non-resolvable): {plan_str}"
    );
    insta::assert_snapshot!(plan, @r###"
    QueryPlan {
      Fetch(service: "SubgraphB") {
        {
          stuff {
            __typename
            data
          }
        }
      },
    }
    "###);
}

// ---------------------------------------------------------------------------
// Type explosion: abstract type conditions decomposed into concrete fragments
// ---------------------------------------------------------------------------

/// Union U = {A, B, C} with `... on I` where I's supergraph runtime types
/// cover all of U (A implements I only in Subgraph2). The condition is
/// vacuous (try_vacuous_type_condition), then the child field `v` on the
/// union has no direct edge, triggering try_explode_interface_field to
/// decompose into per-concrete-type fragments.
#[test]
fn inc_type_explosion_union_interface_interaction() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
          type Query {
            u: U
          }

          union U = A | B | C

          interface I {
            v: Int
          }

          type A {
            v: Int @shareable
          }

          type B implements I {
            v: Int
          }

          type C implements I {
            v: Int
          }
        "#,
        Subgraph2: r#"
          interface I {
            v: Int
          }

          type A implements I {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u {
              ... on I {
                v
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          u {
            __typename
            ... on A {
              v
            }
            ... on B {
              v
            }
            ... on C {
              v
            }
          }
        }
      },
    }
    "###
    );
}

/// Union U = {B, C} where both implement I locally, so `... on I` has a
/// normal query graph edge (downcast). The fragment routes directly without
/// entering the type-condition fallback path. The plan retains the type
/// condition since it gates different runtime behavior for each member.
#[test]
fn inc_all_members_implement_interface_routes_directly() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
          type Query {
            u: U
          }

          union U = B | C

          interface I {
            v: Int
          }

          type A implements I {
            v: Int @shareable
          }

          type B implements I {
            v: Int
          }

          type C implements I {
            v: Int
          }
        "#,
        Subgraph2: r#"
          union U = A

          type A {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u {
              ... on I {
                v
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          u {
            __typename
            ... on I {
              __typename
              v
            }
          }
        }
      },
    }
    "###
    );
}

/// Condition-less inline fragment carrying @skip: the fragment has no type
/// condition (so no query graph edge exists for it), but the @skip directive
/// must be preserved as a condition on the children. The pass-through path
/// pushes children with the directive on the op path.
#[test]
fn inc_conditionless_fragment_skip_preserved() {
    let planner = planner!(
        config = incremental_config(),
        SubgraphA: r#"
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
          query ($hide: Boolean!) {
            user {
              ... @skip(if: $hide) {
                name
                email
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Fetch(service: "SubgraphA") {
        {
          user {
            ... @skip(if: $hide) {
              name
              email
            }
          }
        }
      },
    }
    "###
    );
}

/// Union U = {A, B, C} with `... on I` where only B and C implement I, and
/// the interface lives in a different subgraph from U. Because Subgraph1 has
/// no knowledge of I, there is no downcast edge from U to I in its query
/// graph. The supergraph runtime types of I are {B, C}, which partially
/// overlap U's runtime types {A, B, C}. This triggers
/// `try_explode_abstract_type` to decompose the fragment into per-concrete-type
/// fragments for only the intersection {B, C}, excluding A.
#[test]
fn inc_partial_overlap_explodes_abstract_type() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
          type Query {
            u: U
          }

          union U = A | B | C

          type A {
            w: Int
          }

          type B @key(fields: "id") {
            id: ID!
          }

          type C @key(fields: "id") {
            id: ID!
          }
        "#,
        Subgraph2: r#"
          interface I {
            v: Int
          }

          type B implements I @key(fields: "id") {
            id: ID!
            v: Int
          }

          type C implements I @key(fields: "id") {
            id: ID!
            v: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u {
              ... on I {
                v
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            u {
              __typename
              ... on B {
                __typename
                id
              }
              ... on C {
                __typename
                id
              }
            }
          }
        },
        Flatten(path: "u") {
          Fetch(service: "Subgraph2") {
            {
              ... on B {
                __typename
                id
              }
              ... on C {
                __typename
                id
              }
            } =>
            {
              ... on B {
                v
              }
              ... on C {
                v
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
// @defer: a deferred named fragment spread whose fields live in another
// subgraph. The primary block stays a single root fetch, the deferred block
// gets its own entity fetch, and the fragment's __typename rides along. The
// @defer directive is stripped from subgraph operations since subgraph schemas
// do not define it.
// ---------------------------------------------------------------------------

#[test]
fn inc_defer_named_fragment_spread_across_subgraphs() {
    let planner = planner!(
        config = incremental_defer_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            y: Int
          }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              ...OnT @defer
              x
            }
          }

          fragment OnT on T {
            y
            __typename
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { x } }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                id
                x
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            { ... on T { __typename y } }:
            Parallel {
              Flatten(path: "t") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      y
                    }
                  }
                },
              },
              Flatten(path: "t") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      __typename
                    }
                  }
                },
              },
            },
          },
        ]
      },
    }
    "###
    );
}

/// Nested @defer: the outer deferred block itself contains a @defer,
/// exercising DeferBlockInfo::parent_label, children_of, the recursive
/// branch of build_deferred_blocks, and nested DeferNode wrapping.
#[test]
fn inc_nested_defer_produces_nested_defer_nodes() {
    let planner = planner!(
        config = incremental_defer_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            y: Int
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id") {
            id: ID!
            z: Int
          }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              x
              ... @defer {
                y
                ... @defer {
                  z
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { x } }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                x
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            Defer {
              Primary {
                { y }:
                Flatten(path: "t") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on T {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on T {
                        y
                      }
                    }
                  },
                },
              }, [
                Deferred(depends: [0], path: "t") {
                  { z }:
                  Flatten(path: "t") {
                    Fetch(service: "Subgraph3") {
                      {
                        ... on T {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on T {
                          z
                        }
                      }
                    },
                  },
                },
              ]
            },
          },
        ]
      },
    }
    "###
    );
}

// ---------------------------------------------------------------------------
// @fromContext: context rewrite path includes TypenameEquals guard
// Based on: context.rs::set_context_one_subgraph
// ---------------------------------------------------------------------------

fn parse_fetch_data_path_element(value: &str) -> FetchDataPathElement {
    if value == ".." {
        FetchDataPathElement::Parent
    } else if let Some(("", ty)) = value.split_once("... on ") {
        FetchDataPathElement::TypenameEquals(Name::new(ty).unwrap())
    } else {
        FetchDataPathElement::Key(Name::new(value).unwrap(), Default::default())
    }
}

#[test]
fn context_rewrite_path_includes_typename_equals() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          randomId: ID!
        }
        "#,
    );

    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        r#"
        {
          t {
            u {
              field
            }
          }
        }
        "#,
        "operation.graphql",
    )
    .expect("valid graphql document");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan generated");

    // Extract the context rewrite from the second node (the flatten/fetch).
    let Some(TopLevelPlanNode::Sequence(node)) = &plan.node else {
        panic!("expected sequence node");
    };
    let Some(PlanNode::Flatten(node)) = node.nodes.get(1) else {
        panic!("expected flatten node at index 1");
    };
    let PlanNode::Fetch(fetch) = &*node.node else {
        panic!("expected fetch node inside flatten");
    };

    // The rewrite path must include the TypenameEquals guard:
    // [Parent, TypenameEquals("T"), Key("prop")], not [Parent, Key("prop")].
    assert_eq!(fetch.context_rewrites.len(), 1);
    let FetchDataRewrite::KeyRenamer(renamer) = &*fetch.context_rewrites[0] else {
        panic!("expected KeyRenamer");
    };
    assert_eq!(renamer.rename_key_to.as_str(), "contextualArgument_1_0");

    let expected_path: Vec<FetchDataPathElement> = ["..", "... on T", "prop"]
        .into_iter()
        .map(parse_fetch_data_path_element)
        .collect();
    assert_eq!(
        renamer.path, expected_path,
        "context rewrite path should include TypenameEquals(\"T\")"
    );
}

/// Multi-hop ancestor walking: @context is defined on a grandparent type
/// (A), the @fromContext field lives on type C which is two entity hops
/// below A. The planner must walk parent_types past B to find A, and the
/// rewrite path needs Parent elements for each level of nesting.
#[test]
fn inc_from_context_multi_hop_ancestor() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Query {
          a: A!
        }
        type A @key(fields: "id") @context(name: "ctx") {
          id: ID!
          b: B!
          prop: String!
        }
        type B @key(fields: "id") {
          id: ID!
          c: C!
        }
        type C @key(fields: "id") {
          id: ID!
          value(arg: String @fromContext(field: "$ctx { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          dummy: ID!
        }
        "#,
    );

    // Verify the full plan: A's fetch includes `prop` for context data,
    // and C's entity fetch references $contextualArgument_1_0.
    assert_plan!(
        &planner,
        r#"
        {
          a {
            b {
              c {
                value
              }
            }
          }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            a {
              b {
                c {
                  __typename
                  id
                }
              }
              prop
            }
          }
        },
        Flatten(path: "a.b.c") {
          Fetch(service: "Subgraph1") {
            {
              ... on C {
                __typename
                id
              }
            } =>
            {
              ... on C {
                value(arg: $contextualArgument_1_0)
              }
            }
          },
        },
      },
    }
    "###
    );

    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        r#"
        {
          a {
            b {
              c {
                value
              }
            }
          }
        }
        "#,
        "operation.graphql",
    )
    .expect("valid graphql document");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan generated");

    let Some(TopLevelPlanNode::Sequence(seq)) = &plan.node else {
        panic!("expected sequence node");
    };

    // Find the deepest flatten/fetch (C's entity fetch with the context
    // rewrite).
    let last_flatten = seq.nodes.iter().rev().find_map(|n| {
        if let PlanNode::Flatten(f) = n {
            Some(f)
        } else {
            None
        }
    });
    let Some(flatten) = last_flatten else {
        panic!("expected at least one flatten node");
    };
    let PlanNode::Fetch(fetch) = &*flatten.node else {
        panic!("expected fetch node inside flatten");
    };

    // The rewrite path should walk 2+ Parent elements back to the
    // grandparent A, with a TypenameEquals guard for A.
    assert!(
        !fetch.context_rewrites.is_empty(),
        "context rewrites must be present on the deepest fetch"
    );
    let FetchDataRewrite::KeyRenamer(renamer) = &*fetch.context_rewrites[0] else {
        panic!("expected KeyRenamer");
    };

    let parent_count = renamer
        .path
        .iter()
        .filter(|e| matches!(e, FetchDataPathElement::Parent))
        .count();
    assert!(
        parent_count >= 2,
        "multi-hop context should have at least 2 Parent elements in the rewrite path, got {parent_count}"
    );

    assert!(
        renamer
            .rename_key_to
            .as_str()
            .starts_with("contextualArgument_"),
        "rename key should be a contextualArgument, got {:?}",
        renamer.rename_key_to
    );
}

/// Context rewrites of every fetch in the plan, as (rename_key_to, path).
fn context_rewrite_paths(plan: &QueryPlan) -> Vec<(String, Vec<FetchDataPathElement>)> {
    fn walk(node: &PlanNode, out: &mut Vec<(String, Vec<FetchDataPathElement>)>) {
        match node {
            PlanNode::Fetch(fetch) => {
                for rewrite in &fetch.context_rewrites {
                    if let FetchDataRewrite::KeyRenamer(renamer) = &**rewrite {
                        out.push((renamer.rename_key_to.to_string(), renamer.path.clone()));
                    }
                }
            }
            PlanNode::Flatten(flatten) => walk(&flatten.node, out),
            PlanNode::Sequence(seq) => seq.nodes.iter().for_each(|n| walk(n, out)),
            PlanNode::Parallel(par) => par.nodes.iter().for_each(|n| walk(n, out)),
            other => panic!("unexpected plan node in context test: {other:?}"),
        }
    }
    let mut out = Vec::new();
    match &plan.node {
        Some(TopLevelPlanNode::Fetch(fetch)) => walk(&PlanNode::Fetch(fetch.clone()), &mut out),
        Some(TopLevelPlanNode::Sequence(seq)) => seq.nodes.iter().for_each(|n| walk(n, &mut out)),
        Some(TopLevelPlanNode::Parallel(par)) => par.nodes.iter().for_each(|n| walk(n, &mut out)),
        other => panic!("unexpected top-level plan node: {other:?}"),
    }
    out
}

fn rewrite_path(elements: &[&str]) -> Vec<FetchDataPathElement> {
    elements
        .iter()
        .copied()
        .map(parse_fetch_data_path_element)
        .collect()
}

/// A context field that is @external in the @context subgraph must be
/// fetched from the subgraph that resolves it, not selected where it is
/// external. The search has to back out of the direct append and take the
/// Subgraph2 hop instead.
#[test]
fn inc_from_context_external_field_fetched_from_resolving_subgraph() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String! @external
        }
        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          a: Int!
        }
        type T @key(fields: "id") {
          id: ID!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          t {
            u {
              id
              field
            }
          }
        }
        "#,
        @r###"
               QueryPlan {
                 Sequence {
                   Fetch(service: "Subgraph1") {
                     {
                       t {
                         __typename
                         u {
                           __typename
                           id
                         }
                         id
                       }
                     }
                   },
                   Flatten(path: "t") {
                     Fetch(service: "Subgraph2") {
                       {
                         ... on T {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on T {
                           prop
                         }
                       }
                     },
                   },
                   Flatten(path: "t.u") {
                     Fetch(service: "Subgraph1") {
                       {
                         ... on U {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on U {
                           field(a: $contextualArgument_1_0)
                         }
                       }
                     },
                   },
                 },
               }
               "###
    );
    assert_eq!(
        context_rewrite_paths(&plan),
        vec![(
            "contextualArgument_1_0".to_string(),
            rewrite_path(&["..", "... on T", "prop"])
        )]
    );
}

/// A type carrying the same @context as its @fromContext field must read the
/// context from its nearest ancestor, not from itself: `value` on `child`
/// takes `tree.prop`, so the rewrite needs a Parent step.
#[test]
fn inc_from_context_self_context_reads_from_ancestor() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Query {
          start: Wrapper!
        }
        type Wrapper @key(fields: "id") @context(name: "ctx") {
          id: ID!
          prop: String!
          tree: Tree!
        }
        type Tree @key(fields: "id") @context(name: "ctx") {
          id: ID!
          prop: String!
          child: Tree!
          value(arg: String @fromContext(field: "$ctx { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          dummy: ID!
        }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          start {
            tree {
              child {
                value
              }
            }
          }
        }
        "#,
        @r###"
               QueryPlan {
                 Sequence {
                   Fetch(service: "Subgraph1") {
                     {
                       start {
                         tree {
                           child {
                             __typename
                             id
                           }
                           prop
                         }
                       }
                     }
                   },
                   Flatten(path: "start.tree.child") {
                     Fetch(service: "Subgraph1") {
                       {
                         ... on Tree {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on Tree {
                           value(arg: $contextualArgument_1_0)
                         }
                       }
                     },
                   },
                 },
               }
               "###
    );
    assert_eq!(
        context_rewrite_paths(&plan),
        vec![(
            "contextualArgument_1_0".to_string(),
            rewrite_path(&["..", "... on Tree", "prop"])
        )]
    );
}

/// Type-conditioned context selections name types other than the ancestor
/// actually found, which FieldSet validation rejects. They must still plan,
/// with the rewrite unwrapped to the matching runtime type.
#[test]
fn inc_from_context_type_conditioned_selection_plans() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Query {
          a: A!
        }
        type A @key(fields: "id") {
          id: ID!
        }
        "#,
        Subgraph2: r#"
        type A @key(fields: "id") @context(name: "ctx") {
          id: ID!
          prop: String! @shareable
          child: C!
        }
        type B @key(fields: "id") @context(name: "ctx") {
          id: ID!
          prop: String! @shareable
          child: C!
        }
        type C @key(fields: "id") {
          id: ID!
          value(arg: String @fromContext(field: "$ctx ... on A { prop } ... on B { prop }")): Int!
        }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          a {
            child {
              value
            }
          }
        }
        "#,
        @r###"
               QueryPlan {
                 Sequence {
                   Fetch(service: "Subgraph1") {
                     {
                       a {
                         __typename
                         id
                       }
                     }
                   },
                   Flatten(path: "a") {
                     Fetch(service: "Subgraph2") {
                       {
                         ... on A {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on A {
                           child {
                             __typename
                             id
                           }
                           prop
                         }
                       }
                     },
                   },
                   Flatten(path: "a.child") {
                     Fetch(service: "Subgraph2") {
                       {
                         ... on C {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on C {
                           value(arg: $contextualArgument_2_0)
                         }
                       }
                     },
                   },
                 },
               }
               "###
    );
    assert_eq!(
        context_rewrite_paths(&plan),
        vec![(
            "contextualArgument_2_0".to_string(),
            rewrite_path(&["..", "... on A", "prop"])
        )]
    );
}

/// A key hop into Subgraph1 lands on `tree`, whose type carries the same
/// @context as `value`. `value` on `child` must read `tree.prop` from the
/// hop's own fetch, not treat the boundary type as already providing it.
#[test]
fn inc_from_context_boundary_entity_with_self_context_reads_from_ancestor() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
        type Wrapper @key(fields: "id") @context(name: "ctx") {
          id: ID!
          prop: String!
          tree: Tree!
        }
        type Tree @key(fields: "id") @context(name: "ctx") {
          id: ID!
          prop: String!
          child: Tree!
          value(arg: String @fromContext(field: "$ctx { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          start: Wrapper!
        }
        type Wrapper @key(fields: "id") {
          id: ID!
        }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          start {
            tree {
              child {
                value
              }
            }
          }
        }
        "#,
        @r###"
               QueryPlan {
                 Sequence {
                   Fetch(service: "Subgraph2") {
                     {
                       start {
                         __typename
                         id
                       }
                     }
                   },
                   Flatten(path: "start") {
                     Fetch(service: "Subgraph1") {
                       {
                         ... on Wrapper {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on Wrapper {
                           tree {
                             child {
                               __typename
                               id
                             }
                             prop
                           }
                         }
                       }
                     },
                   },
                   Flatten(path: "start.tree.child") {
                     Fetch(service: "Subgraph1") {
                       {
                         ... on Tree {
                           __typename
                           id
                         }
                       } =>
                       {
                         ... on Tree {
                           value(arg: $contextualArgument_1_0)
                         }
                       }
                     },
                   },
                 },
               }
               "###
    );
    assert_eq!(
        context_rewrite_paths(&plan),
        vec![(
            "contextualArgument_1_0".to_string(),
            rewrite_path(&["..", "... on Tree", "prop"])
        )]
    );
}

/// Reaching Z takes a key-hop chain. Every intermediate subgraph starts a
/// candidate chain, and BULB must pick the shortest one, S1 -> Y1 -> Z as
/// legacy does, not a detour through S2 that adds a no-op fetch.
#[test]
fn inc_interface_object_type_preserving_transitions_skip_noop_hop() {
    let planner = planner!(
        config = incremental_config(),
        S1: r#"
            type A @key(fields: "id") {
                id: ID!
            }

            type Query {
                test: A
            }
        "#,
        S2: r#"
            type A @key(fields: "id") {
                id: ID!
            }
        "#,
        S3: r#"
            type A @key(fields: "id") {
                id: ID!
            }
        "#,
        S4: r#"
            type A @key(fields: "id") {
                id: ID!
            }
        "#,
        Y1: r#"
            interface I {
                id: ID!
            }

            type A implements I @key(fields: "id") @key(fields: "alt_id { id }") {
                id: ID!
                alt_id: AltID!
            }

            type AltID {
                id: ID!
            }
        "#,
        Y2: r#"
            interface I {
                id: ID!
            }

            type A implements I @key(fields: "id") @key(fields: "alt_id { id }") {
                id: ID!
                alt_id: AltID!
            }

            type AltID {
                id: ID!
            }
        "#,
        Z: r#"
            type I @interfaceObject @key(fields: "alt_id { id }") {
                alt_id: AltID!
                data: String!
            }

            type AltID {
                id: ID!
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            {
                test {
                    data
                }
            }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "S1") {
          {
            test {
              __typename
              id
            }
          }
        },
        Flatten(path: "test") {
          Fetch(service: "Y1") {
            {
              ... on A {
                __typename
                id
              }
            } =>
            {
              ... on A {
                __typename
                alt_id {
                  id
                }
              }
            }
          },
        },
        Flatten(path: "test") {
          Fetch(service: "Z") {
            {
              ... on A {
                __typename
                alt_id {
                  id
                }
              }
            } =>
            {
              ... on I {
                data
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
fn inc_interface_type_explosion_routes_value_type_field() {
    let planner = planner!(
        config = incremental_config(),
        Subgraph1: r#"
          type Query {
            i: I
          }
          interface I {
            s: S
          }
          type T implements I @key(fields: "id") {
            id: ID!
            s: S @shareable
          }
          type S @shareable {
            x: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            s: S @shareable
          }
          type S @shareable {
            x: Int
            y: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i {
              s {
                y
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                i {
                  __typename
                  ... on T {
                    __typename
                    id
                  }
                }
              }
            },
            Flatten(path: "i") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    s {
                      y
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

/// specifically rides the incoming inputs. Without the rides_representation
/// check the planner would try to re-route `id` as a condition pending and
/// create a circular ordering dependency.
#[test]
fn inc_rides_representation_skips_redundant_key_condition_routing() {
    let planner = planner!(
        config = incremental_config(),
        A: r#"
          type Query { t: T }
          type T @key(fields: "id") { id: ID! }
        "#,
        B: r#"
          type T @key(fields: "id") {
            id: ID!
            name: String @shareable
          }
        "#,
        C: r#"
          type T @key(fields: "id name") {
            id: ID!
            name: String @shareable
            detail: String
          }
        "#
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              detail
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
                name
              }
            }
          },
        },
        Flatten(path: "t") {
          Fetch(service: "C") {
            {
              ... on T {
                __typename
                id
                name
              }
            } =>
            {
              ... on T {
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

/// Forced backtracking within fast_forward recovers from a dead-end
/// circular-key commit by rewinding an ancestor forced commit and trying
/// the next alternative. This differs from BULB backtracking: forced
/// commits have no decision frame, so without the trail mechanism the
/// planner would permanently drop the field.
///
/// Schema: E in A (key: id, has `c: C`), E in T (key: "c { cid cm }",
///         has `target`). C in A (has `cid`), C in B (has `cid, cm`).
/// Condition `c.cm` is only in B. The greedy first choice routes `c`
/// through A (closer), but A cannot supply `cm` for the circular key.
/// The forced trail rewinds `c` to B where the full key resolves.
///
/// This is the same schema as inc_circular_key_backtracks_to_alternative
/// but asserts the exact plan shape to pin the forced-backtracking path.
#[test]
fn inc_forced_backtrack_recovers_circular_key_dead_end() {
    let supergraph_sdl = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
{
  query: Query
}

directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "a", url: "http://a")
  B @join__graph(name: "b", url: "http://b")
  T @join__graph(name: "t", url: "http://t")
}

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query
  @join__type(graph: A)
{
  entry: E @join__field(graph: A)
}

type E
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: T, key: "c { cid cm }")
{
  id: ID! @join__field(graph: A) @join__field(graph: B)
  c: C @join__field(graph: A) @join__field(graph: B) @join__field(graph: T)
  target: String @join__field(graph: T)
}

type C
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: T, key: "cid cm")
{
  cid: ID! @join__field(graph: A) @join__field(graph: B) @join__field(graph: T)
  cm: String @join__field(graph: B) @join__field(graph: T)
}
"#;
    let supergraph = apollo_federation::Supergraph::new(supergraph_sdl).expect("valid supergraph");
    let planner = apollo_federation::query_plan::query_planner::QueryPlanner::new(
        &supergraph,
        incremental_config(),
    )
    .expect("can create query planner");
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        "{ entry { target } }",
        "test.graphql",
    )
    .expect("valid graphql document");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("forced backtracking should recover from the dead-end circular key");
    let plan_str = plan.to_string();
    // The key's `c` subtree must route through B, not A.
    assert!(
        plan_str.contains("service: \"b\""),
        "Plan should route the key conditions through subgraph b: {plan_str}"
    );
    assert!(
        plan_str.contains("target"),
        "Plan should fetch 'target' from T: {plan_str}"
    );
    // The plan must NOT mention subgraph a for any entity fetch beyond
    // the root query, confirming the forced trail rewound past A.
    let entity_fetches: Vec<&str> = plan_str
        .lines()
        .filter(|l| l.contains("Fetch(service:") && !l.contains("\"a\""))
        .collect();
    assert!(
        !entity_fetches.is_empty(),
        "There should be non-A entity fetches: {plan_str}"
    );
}

// A plain @requires input arriving after an aliased one on the same edge
// must not overwrite the plain input at runtime. The KeyRenamer's
// remove-then-insert replaces whatever sits under the original name, so
// both inputs cannot share a single entity group.
#[test]
fn inc_plain_requires_after_aliased_requires_does_not_overwrite() {
    let planner = planner!(
        config = incremental_config(),
        S1: r#"
        type Query { t: T }
        type T @key(fields: "id") { id: ID!  a: A }
        type A @key(fields: "id") { id: ID!  y: Int }
        "#,
        S3: r#"
        type A @key(fields: "id") { id: ID!  x: Int }
        "#,
        S2: r#"
        type T @key(fields: "id") {
            id: ID!
            a: A @external
            b: Int @requires(fields: "a { x }")
            c: Int @requires(fields: "a { y }")
        }
        type A @key(fields: "id") {
            id: ID!
            x: Int @external
            y: Int @external
        }
        "#,
    );
    // b's requires needs S3 (aliased), c's requires is resolvable from S1
    // (plain). Both orderings must produce a valid plan.
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    let _plan_bc = assert_plan!(
        validate_correctness = false,
        &planner,
        "{ t { b c } }",
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "S1") {
          {
            t {
              __typename
              id
              __require_0_a: a {
                __typename
                id
              }
              __require_1_a: a {
                y
              }
            }
          }
        },
        Parallel {
          Flatten(path: "t") {
            Fetch(service: "S2") {
              {
                ... on T {
                  __typename
                  id
                  __require_1_a: a {
                    y
                  }
                }
              } =>
              {
                ... on T {
                  c
                }
              }
            },
          },
          Flatten(path: "t.__require_0_a") {
            Fetch(service: "S3") {
              {
                ... on A {
                  __typename
                  id
                }
              } =>
              {
                ... on A {
                  x
                }
              }
            },
          },
        },
        Flatten(path: "t") {
          Fetch(service: "S2") {
            {
              ... on T {
                __typename
                id
                __require_0_a: a {
                  x
                }
              }
            } =>
            {
              ... on T {
                b
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
// Same-subgraph @defer: deferred field must get its own entity fetch
// ---------------------------------------------------------------------------

#[test]
fn inc_defer_same_subgraph_produces_entity_fetch() {
    let planner = planner!(
        config = incremental_defer_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
            v1: String
          }
        "#,
    );

    // v1 is deferred, so the primary fetch must NOT include v1.
    // The Deferred block must contain its own entity fetch for v1.
    assert_plan!(planner,
        r#"
          {
            t {
              v0
              ... @defer {
                v1
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { v0 } }:
          Fetch(service: "Subgraph1", id: 0) {
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
              Fetch(service: "Subgraph1") {
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
    "###
    );
}

/// Mutation with same-subgraph @defer: BULB plans each top-level mutation
/// field independently and sequences the resulting Defer nodes. Each
/// deferred field must get its own entity fetch rather than riding the
/// primary mutation fetch.
#[test]
fn inc_defer_on_mutation_in_same_subgraph() {
    let planner = planner!(
        config = incremental_defer_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type Mutation {
            update1: T
            update2: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
            v1: String
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          mutation mut {
            update1 {
              v0
              ... @defer {
                v1
              }
            }
            update2 {
              v1
              ... @defer {
                v0
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Defer {
          Primary {
            { update1 { v0 } }:
            Fetch(service: "Subgraph1", id: 0) {
              {
                update1 {
                  __typename
                  v0
                  id
                }
              }
            },
          }, [
            Deferred(depends: [0], path: "update1") {
              { v1 }:
              Flatten(path: "update1") {
                Fetch(service: "Subgraph1") {
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
        Defer {
          Primary {
            { update2 { v1 } }:
            Fetch(service: "Subgraph1", id: 1) {
              {
                update2 {
                  __typename
                  v1
                  id
                }
              }
            },
          }, [
            Deferred(depends: [1], path: "update2") {
              { v0 }:
              Flatten(path: "update2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      v0
                    }
                  }
                },
              },
            },
          ]
        },
      },
    }
    "###
    );
}

/// Multi-dependency deferred section: the deferred field's key (id1 id2)
/// is only resolvable via fetches to intermediate subgraphs.
#[test]
fn inc_defer_multi_dependency_deferred_section() {
    let planner = planner!(
        config = incremental_defer_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id0") {
            id0: ID!
            v1: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id0") @key(fields: "id1") {
            id0: ID!
            id1: ID!
            v2: Int
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id0") @key(fields: "id2") {
            id0: ID!
            id2: ID!
            v3: Int
          }
        "#,
        Subgraph4: r#"
          type T @key(fields: "id1 id2") {
            id1: ID!
            id2: ID!
            v4: Int
          }
        "#,
    );

    assert_plan!(&planner,
        r#"
          {
            t {
              v1
              v2
              v3
              ... @defer {
                v4
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { v1 v2 v3 } }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                t {
                  __typename
                  v1
                  id0
                }
              }
            },
            Parallel {
              Flatten(path: "t") {
                Fetch(service: "Subgraph3", id: 1) {
                  {
                    ... on T {
                      __typename
                      id0
                    }
                  } =>
                  {
                    ... on T {
                      v3
                      id2
                    }
                  }
                },
              },
              Flatten(path: "t") {
                Fetch(service: "Subgraph2", id: 2) {
                  {
                    ... on T {
                      __typename
                      id0
                    }
                  } =>
                  {
                    ... on T {
                      v2
                      id1
                    }
                  }
                },
              },
            },
          },
        }, [
          Deferred(depends: [1, 2, 0], path: "t") {
            { v4 }:
            Flatten(path: "t") {
              Fetch(service: "Subgraph4") {
                {
                  ... on T {
                    __typename
                    id1
                    id2
                  }
                } =>
                {
                  ... on T {
                    v4
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

/// Ordering pair for aliased/plain @requires inputs on one entity edge:
/// b's condition (a { x }) must be staged and aliased, c's (a { y }) is
/// resolvable in place and stays plain. Whichever arrives second must not
/// share a representation with the other, since the KeyRenamer's
/// remove-then-insert would overwrite the plain `a` at runtime.
#[test]
fn inc_requires_aliased_then_plain_input_does_not_collide() {
    let planner = planner!(
        config = incremental_config(),
        S1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: A
          }

          type A @key(fields: "id") {
            id: ID!
            y: Int
          }
        "#,
        S2: r#"
          type T @key(fields: "id") {
            id: ID!
            a: A @external
            b: Int @requires(fields: "a { x }")
            c: Int @requires(fields: "a { y }")
          }

          type A @key(fields: "id") {
            id: ID! @external
            x: Int @external
            y: Int @external
          }
        "#,
        S3: r#"
          type A @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
    );

    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"{ t { b c } }"#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "S1") {
          {
            t {
              __typename
              id
              __require_0_a: a {
                __typename
                id
              }
              __require_1_a: a {
                y
              }
            }
          }
        },
        Parallel {
          Flatten(path: "t") {
            Fetch(service: "S2") {
              {
                ... on T {
                  __typename
                  id
                  __require_1_a: a {
                    y
                  }
                }
              } =>
              {
                ... on T {
                  c
                }
              }
            },
          },
          Flatten(path: "t.__require_0_a") {
            Fetch(service: "S3") {
              {
                ... on A {
                  __typename
                  id
                }
              } =>
              {
                ... on A {
                  x
                }
              }
            },
          },
        },
        Flatten(path: "t") {
          Fetch(service: "S2") {
            {
              ... on T {
                __typename
                id
                __require_0_a: a {
                  x
                }
              }
            } =>
            {
              ... on T {
                b
              }
            }
          },
        },
      },
    }
    "###
    );

    assert_plan!(&planner,
        r#"{ t { c b } }"#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "S1") {
          {
            t {
              __typename
              id
              a {
                __typename
                y
                id
              }
            }
          }
        },
        Flatten(path: "t.a") {
          Fetch(service: "S3") {
            {
              ... on A {
                __typename
                id
              }
            } =>
            {
              ... on A {
                x
              }
            }
          },
        },
        Flatten(path: "t") {
          Fetch(service: "S2") {
            {
              ... on T {
                __typename
                id
                a {
                  y
                  x
                }
              }
            } =>
            {
              ... on T {
                c
                b
              }
            }
          },
        },
      },
    }
    "###
    );
}

/// Identical @requires conditions intern to one alias so their consumers
/// share a single entity fetch; the rewrite-conflict check must compare
/// aliases, not just originals, or the second consumer splits needlessly.
#[test]
fn inc_requires_identical_conditions_share_one_fetch() {
    let planner = planner!(
        config = incremental_config(),
        S1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        S2: r#"
          type T @key(fields: "id") {
            id: ID!
            a: Int @external
            b: Int @requires(fields: "a")
            c: Int @requires(fields: "a")
          }
        "#,
        S3: r#"
          type T @key(fields: "id") {
            id: ID!
            a: Int
          }
        "#,
    );

    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"{ t { b c } }"#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "S1") {
          {
            t {
              __typename
              id
            }
          }
        },
        Flatten(path: "t") {
          Fetch(service: "S3") {
            {
              ... on T {
                __typename
                id
              }
            } =>
            {
              ... on T {
                __require_0_a: a
              }
            }
          },
        },
        Flatten(path: "t") {
          Fetch(service: "S2") {
            {
              ... on T {
                __typename
                id
                __require_0_a: a
              }
            } =>
            {
              ... on T {
                b
                c
              }
            }
          },
        },
      },
    }
    "###
    );
}

/// Overlapping @requires conditions (foo needs a subset of bar's) share
/// one alias via containment interning, so the common `v { y { isY } }`
/// chain is staged once instead of once per consumer. Legacy plans this
/// with 6 fetches; BULB currently uses 7.
#[test]
fn inc_requires_overlapping_conditions_fetch_count() {
    let planner = planner!(
        config = incremental_config(),
        s1: r#"
            type Query {
              t: T
            }

            interface I {
              id: ID!
              name: String!
            }

            type T implements I @key(fields: "id") {
              id: ID!
              name: String! @shareable
              x: X @shareable
              v: V @shareable
            }

            type U implements I @key(fields: "id") {
              id: ID!
              name: String! @external
            }

            type V @key(fields: "id") @key(fields: "internalID") {
              id: ID!
              internalID: ID!
            }

            type X @key(fields: "t { id }") {
              t: T!
              isX: Boolean!
            }
        "#,
        s2: r#"
            type V @key(fields: "id") {
              id: ID!
              internalID: ID! @shareable
              y: Y! @shareable
              zz: [Z!] @external
            }

            type Z {
              u: U! @external
            }

            type Y @key(fields: "id") {
              id: ID!
              isY: Boolean! @external
            }

            interface I {
              id: ID!
              name: String!
            }

            type T implements I @key(fields: "id") {
              id: ID!
              name: String! @external
              x: X @external
              v: V @external
              foo: [String!]! @requires(fields: "x { isX }\nv { y { isY } }")
              bar: [I!]! @requires(fields: "x { isX }\nv { y { isY } zz { u { id } } }")
            }

            type X {
              isX: Boolean! @external
            }

            type U implements I @key(fields: "id") {
              id: ID!
              name: String! @external
            }
        "#,
        s3: r#"
            type V @key(fields: "internalID") {
              internalID: ID!
              y: Y! @shareable
            }

            type Y @key(fields: "id") {
              id: ID!
              isY: Boolean!
            }
        "#,
        s4: r#"
            type V @key(fields: "id") @key(fields: "internalID") {
              id: ID!
              internalID: ID!
              zz: [Z!] @override(from: "s1")
            }

            type Z {
              free: Boolean
              u: U!
              v: V!
            }

            interface I {
              id: ID!
              name: String!
            }

            type T implements I @key(fields: "id") {
              id: ID!
              name: String! @shareable
              x: X @shareable
              v: V @shareable
            }

            type X @key(fields: "t { id }", resolvable: false) {
              t: T! @external
            }

            type U implements I @key(fields: "id") {
              id: ID!
              name: String! @override(from: "s1")
            }
        "#,
    );
    // validate_correctness = false: the correctness checker rejects input
    // KeyRenamer rewrites, which this plan uses to rename an aliased
    // @requires condition back to its field name.
    assert_plan!(
        validate_correctness = false,
        &planner,
        r#"
        {
            t {
                foo
                bar {
                    name
                }
            }
        }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "s1") {
          {
            t {
              __typename
              id
              __require_0_x: x {
                isX
              }
              __require_1_v: v {
                __typename
                id
              }
            }
          }
        },
        Parallel {
          Flatten(path: "t.__require_1_v") {
            Fetch(service: "s4") {
              {
                ... on V {
                  __typename
                  id
                }
              } =>
              {
                ... on V {
                  zz {
                    u {
                      id
                    }
                  }
                }
              }
            },
          },
          Sequence {
            Flatten(path: "t.__require_1_v") {
              Fetch(service: "s2") {
                {
                  ... on V {
                    __typename
                    id
                  }
                } =>
                {
                  ... on V {
                    y {
                      __typename
                      id
                    }
                  }
                }
              },
            },
            Flatten(path: "t.__require_1_v.y") {
              Fetch(service: "s3") {
                {
                  ... on Y {
                    __typename
                    id
                  }
                } =>
                {
                  ... on Y {
                    isY
                  }
                }
              },
            },
          },
        },
        Flatten(path: "t") {
          Fetch(service: "s2") {
            {
              ... on T {
                __typename
                id
                __require_0_x: x {
                  isX
                }
                __require_1_v: v {
                  y {
                    isY
                  }
                  zz {
                    u {
                      id
                    }
                  }
                }
              }
            } =>
            {
              ... on T {
                foo
                bar {
                  __typename
                  ... on U {
                    __typename
                    id
                  }
                  ... on T {
                    __typename
                    id
                  }
                }
              }
            }
          },
        },
        Parallel {
          Flatten(path: "t.bar.@|[T]") {
            Fetch(service: "s1") {
              {
                ... on T {
                  __typename
                  id
                }
              } =>
              {
                ... on T {
                  name
                }
              }
            },
          },
          Flatten(path: "t.bar.@|[U]") {
            Fetch(service: "s4") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  name
                }
              }
            },
          },
        },
      },
    }
    "###
    );
}

/// Multiple non-nested @defer labels: sibling entity merging must not
/// combine fetches from different defer scopes; primary data must stay in
/// the primary and each label's data in its own Deferred block.
#[test]
fn inc_defer_multiple_labels_keep_scopes_separate() {
    let planner = planner!(
        config = incremental_defer_config(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
            v1: String
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v2: String
            v3: U
          }

          type U @key(fields: "id") {
            id: ID!
          }
        "#,
        Subgraph3: r#"
          type U @key(fields: "id") {
            id: ID!
            x: Int
            y: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              v0
              ... @defer(label: "defer_v1") {
                v1
              }
              ... @defer {
                v2
              }
              v3 {
                x
                ... @defer(label: "defer_in_v3") {
                  y
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          { t { v0 v3 { x } } }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                t {
                  __typename
                  v0
                  id
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2", id: 1) {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v3 {
                      __typename
                      id
                    }
                  }
                }
              },
            },
            Flatten(path: "t.v3") {
              Fetch(service: "Subgraph3") {
                {
                  ... on U {
                    __typename
                    id
                  }
                } =>
                {
                  ... on U {
                    x
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [1], path: "t/v3", label: "defer_in_v3") {
            { y }:
            Flatten(path: "t.v3") {
              Fetch(service: "Subgraph3") {
                {
                  ... on U {
                    __typename
                    id
                  }
                } =>
                {
                  ... on U {
                    y
                  }
                }
              },
            },
          },
          Deferred(depends: [0], path: "t") {
            { v2 }:
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v2
                  }
                }
              },
            },
          },
          Deferred(depends: [0], path: "t", label: "defer_v1") {
            { v1 }:
            Flatten(path: "t") {
              Fetch(service: "Subgraph1") {
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
    "###
    );
}
