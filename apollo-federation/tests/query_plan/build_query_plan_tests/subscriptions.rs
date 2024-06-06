use apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

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
          Primary: {Fetch(service: "SubgraphA") {
                  subscription MySubscription__SubgraphA__0 {
              onNewUser {
                __typename
                id
                name
              }
            }
          },},
          Rest: {Sequence {
            Flatten(path: "onNewUser") {
              Fetch(service: "SubgraphB") {
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
          },},
        },
      }
      "###
    );
}

#[test]
fn basic_subscription_with_single_subgraph() {
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
          Primary: {Fetch(service: "SubgraphA") {
                  subscription MySubscription__SubgraphA__0 {
              onNewUser {
                id
                name
              }
            }
          },},
        },
      }
      "###
    );
}

#[test]
// TODO: This panic should say something along the line os "@defer is not supported on subscriptions" but
// defer is currently `todo!`. Change this error message once defer is implemented.
#[should_panic(expected = "not yet implemented: @defer not implemented")]
fn trying_to_use_defer_with_a_subcription_results_in_an_error() {
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
