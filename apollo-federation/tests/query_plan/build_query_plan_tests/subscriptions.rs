use apollo_compiler::name;
use apollo_compiler::ExecutableDocument;
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
          Primary: {
            Fetch(service: "SubgraphA") {
              {
                onNewUser {
                  __typename
                  id
                  name
                }
              }
            },
          },
          Rest: {
            Sequence {
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
            },
          },
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
          Primary: {
            Fetch(service: "SubgraphA") {
              {
                onNewUser {
                  id
                  name
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
fn trying_to_use_defer_with_a_subcription_results_in_an_error() {
    let config = QueryPlannerConfig {
        incremental_delivery: QueryPlanIncrementalDeliveryConfig { enable_defer: true },
        ..Default::default()
    };
    let (api_schema, planner) = planner!(
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

    let document = ExecutableDocument::parse_and_validate(
        api_schema.schema(),
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
        "trying_to_use_defer_with_a_subcription_results_in_an_error.graphql",
    )
    .unwrap();

    planner
        .build_query_plan(&document, Some(name!(MySubscription)), Default::default())
        .expect_err("should return an error");
}
