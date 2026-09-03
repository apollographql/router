use apollo_compiler::Name;
use apollo_federation::query_plan::FetchDataPathElement;
use apollo_federation::query_plan::FetchDataRewrite;
use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::query_plan::query_planner::IncrementalPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
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
// operations; the correctness checker must accept these renames.
// Based on: requires.rs::it_handles_simple_require_chain
#[test]
fn inc_requires_rename_correctness_check() {
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
    assert_plan!(
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
    assert_plan!(
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
// Reproduces the customer issue in TSH-23186.
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
    // validate_correctness = false: the correctness checker's KeyRenamer
    // doesn't yet handle the case where the rename target (`p`) already
    // exists with different arguments.
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
    assert_plan!(
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
// @defer: deferred fragment across subgraphs produces a Defer plan node
// with primary and deferred blocks. The @defer directive is stripped from
// subgraph operations since subgraph schemas do not define it.
// ---------------------------------------------------------------------------

#[test]
fn inc_defer_cross_subgraph_produces_defer_plan() {
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
