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

mod fetch_operation_names;
mod provides;
mod requires;
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

/// This tests the issue from https://github.com/apollographql/federation/issues/1858.
/// That issue, which was a bug in the handling of selection sets, was concretely triggered with
/// a mix of an interface field implemented with some covariance and the query plan using
/// type-explosion.
/// That error can be reproduced on a pure fed2 example, it's just a bit more
/// complex as we need to involve a @provide just to force the query planner to type explode
/// (more precisely, this force the query planner to _consider_ type explosion; the generated
/// query plan still ends up not type-exploding in practice since as it's not necessary).
#[test]
fn field_covariance_and_type_explosion() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          dummy: Interface
        }

        interface Interface {
          field: Interface
        }

        type Object implements Interface @key(fields: "id") {
          id: ID!
          field: Object @provides(fields: "x")
          x: Int @external
        }
        "#,
        Subgraph2: r#"
        type Object @key(fields: "id") {
          id: ID!
          x: Int @shareable
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          dummy {
            field {
              ... on Object {
                field {
                  __typename
                }
              }
            }
          }
        }
        "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          dummy {
            field {
              __typename
              ... on Object {
                field {
                  __typename
                }
              }
            }
          }
        }
      },
    }
    "###
    );
}
