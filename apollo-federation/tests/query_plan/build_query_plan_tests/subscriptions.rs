use apollo_federation::query_plan::query_planner::{
    QueryPlanIncrementalDeliveryConfig, QueryPlannerConfig,
};

#[test]
fn basic_subscription_query_plan() {
    let planner = planner!(
    SubgraphA: r#"
            type Query {
                me: User!
            }

            type Subscription {
                onNewUser: User!
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#,
    SubgraphB: r#"
            type Query {
                foo: Int
            }

            type User @key(fields: "id") {
                id: ID!
                address: String!
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        subscription MySubscription {
          onNewUser {
            id
            name
            address
          }
        }
        "#,
        @r###"
        QueryPlan {
            Subscription {
            Primary: {
                Fetch(service: "subgraphA") {
                {
                    onNewUser {
                    __typename
                    id
                    name
                    }
                }
                }
            },
            Rest: {
                Sequence {
                Flatten(path: "onNewUser") {
                    Fetch(service: "subgraphB") {
                    {
                        ... on User {
                        __typename
                        id
                        }
                    } =>
                    {
                        ... on User {
                        address
                        }
                    }
                    },
                },
                }
            }
            },
        }
      "###
    );
}

#[test]
fn basic_subscription_query_plan_single_subgraph() {
    let planner = planner!(
    SubgraphA: r#"
        type Query {
            me: User!
        }

        type Subscription {
            onNewUser: User!
        }

        type User @key(fields: "id") {
            id: ID!
            name: String!
        }
        "#,
    SubgraphB: r#"
        type Query {
            foo: Int
        }

        type User @key(fields: "id") {
            id: ID!
            address: String!
        }
    "#);
    assert_plan!(
        &planner,
        r#"
        subscription MySubscription {
          onNewUser {
            id
            name
          }
        }
        "#,
        @r###"
      QueryPlan {
        Subscription {
          Primary: {
            Fetch(service: "subgraphA") {
              {
                onNewUser {
                  id
                  name
                }
              }
            }
          },
          }
        },
      "###
    );
}

#[test]
#[should_panic(expected = "@defer is not supported on subscriptions")]
fn trying_to_use_defer_with_a_description_results_in_an_error() {
    let config = QueryPlannerConfig {
        incremental_delivery: QueryPlanIncrementalDeliveryConfig { enable_defer: true },
        ..Default::default()
    };
    let planner = planner!(
        config = config,
    SubgraphA: r#"
        type Query {
          me: User!
        }

        type Subscription {
          onNewUser: User!
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
        }
    "#,
    SubgraphB: r#"
        type Query {
          foo: Int
        }

        type User @key(fields: "id") {
          id: ID!
          address: String!
        }
    "#);
    assert_plan!(
        &planner,
        r#"
        subscription MySubscription {
          onNewUser {
            id
            ... @defer {
              name
            }
            address
          }
        }
        "#,
        // This is just a placeholder. We expect the planner to return an Err, which is then
        // unwrapped.
        @r###"
      QueryPlan {
        Subscription {
          Primary: {
            Fetch(service: "subgraphA") {
              {
                onNewUser {
                  id
                  name
                }
              }
            }
          },
          }
        },
      "###
    );
}
