use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

fn config_with_defer() -> QueryPlannerConfig {
    let mut config = QueryPlannerConfig::default();
    config.incremental_delivery.enable_defer = true;
    config
}
#[test]
fn defer_test_reproduction_works() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
        type Price {
          id: ID! @shareable
        }

        type Query {
          products(start: Int = 0, limit: Int = 3): [Product]
        }

        type Product {
          id: ID! @shareable
          name: String @shareable
          price: Price @shareable
        }
        "#,
        Subgraph2: r#"
        type Price {
          id: ID! @shareable
        }

        type Query {
          product(id: ID!): Product
        }

        type Product @key(fields: "id") {
          id: ID! @shareable
          name: String @shareable
          price: Price @shareable
        }
        "#,
        Subgraph3: r#"
        type Price @key(fields: "id") {
          amount: Float
          id: ID! @shareable
        }

        type Query {
          price(id: ID!): Price
        }
        "#,
    );
    assert_plan!(planner,
        r#"
          {
            products {
              id
              name
              price {
                amount
              }
            }
          }
        "#,
        @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            products {
              id
              name
              price {
                __typename
                id
              }
            }
          }
        },
        Flatten(path: "products.@.price") {
          Fetch(service: "Subgraph3") {
            {
              ... on Price {
                __typename
                id
              }
            } =>
            {
              ... on Price {
                amount
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn defer_test_reproduction() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
        type Price {
          id: ID! @shareable
        }

        type Query {
          products(start: Int = 0, limit: Int = 3): [Product]
        }

        type Product {
          id: ID! @shareable
          name: String @shareable
          price: Price @shareable
        }
        "#,
        Subgraph2: r#"
        type Price {
          id: ID! @shareable
        }

        type Query {
          product(id: ID!): Product

        }

        type Product @key(fields: "id", resolvable: true) {
          id: ID! @shareable
          name: String @shareable
          price: Price @shareable
        }
        "#,
        Subgraph3: r#"
        type Price @key(fields: "id", resolvable: true) {
          amount: Float
          id: ID! @shareable
        }

        type Query {
          price(id: ID!): Price
        }
        "#,
    );
    assert_plan!(planner,
        r#"
          {
            products {
              id
              name
              ... @defer {
                price {
                  amount
                }
              }
            }
          }
        "#,
        @"");
}
