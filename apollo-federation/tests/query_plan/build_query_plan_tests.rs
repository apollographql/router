/*
Template to copy-paste:

#[test]
fn some_name() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            ...
          }
        "#,
        Subgraph2: r#"
          type Query {
            ...
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            ...
          }
        "#,
        @r###"
          QueryPlan {
            ...
          }
        "###
    );
}
*/

mod shareable_root_fields;

// TODO: port the rest of query-planner-js/src/__tests__/buildPlan.test.ts

#[test]
fn pick_keys_that_minimize_fetches() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            transfers: [Transfer!]!
          }

          type Transfer @key(fields: "from { iso } to { iso }") {
            from: Country!
            to: Country!
          }

          type Country @key(fields: "iso") {
            iso: String!
          }
        "#,
        Subgraph2: r#"
          type Transfer @key(fields: "from { iso } to { iso }") {
            id: ID!
            from: Country!
            to: Country!
          }

          type Country @key(fields: "iso") {
            iso: String!
            currency: Currency!
          }

          type Currency {
            name: String!
            sign: String!
          }
        "#,
    );
    // We want to make sure we use the key on Transfer just once,
    // not 2 fetches using the keys on Country.
    assert_plan!(
        &planner,
        r#"
          {
            transfers {
              from {
                currency {
                  name
                }
              }
              to {
                currency {
                  sign
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
                  transfers {
                    __typename
                    from {
                      iso
                    }
                    to {
                      iso
                    }
                  }
                }
              },
              Flatten(path: "transfers.@") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on Transfer {
                      __typename
                      from {
                        iso
                      }
                      to {
                        iso
                      }
                    }
                  } =>
                  {
                    ... on Transfer {
                      from {
                        currency {
                          name
                        }
                      }
                      to {
                        currency {
                          sign
                        }
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
