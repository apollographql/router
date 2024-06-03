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
mod named_fragments_preservation;
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

#[test]
#[should_panic(expected = "not yet implemented")]
// TODO: investigate this failure
fn handles_non_intersecting_fragment_conditions() {
    let planner = planner!(
        Subgraph1: r#"
            interface Fruit {
              edible: Boolean!
            }
    
            type Banana implements Fruit {
              edible: Boolean!
              inBunch: Boolean!
            }
    
            type Apple implements Fruit {
              edible: Boolean!
              hasStem: Boolean!
            }
    
            type Query {
              fruit: Fruit!
            }
          "#,
    );
    assert_plan!(
        &planner,
        r#"
            fragment OrangeYouGladIDidntSayBanana on Fruit {
              ... on Banana {
                inBunch
              }
              ... on Apple {
                hasStem
              }
            }
    
            query Fruitiness {
              fruit {
                ... on Apple {
                  ...OrangeYouGladIDidntSayBanana
                }
              }
            }
          "#,
          @r#"
          QueryPlan {
            Fetch(service: "Subgraph1") {
              {
                fruit {
                  __typename
                  ... on Apple {
                    hasStem
                  }
                }
              }
            },
          }
          "#
    );
}

#[test]
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn avoids_unnecessary_fetches() {
    // This test is a reduced example demonstrating a previous issue with the computation of query plans cost.
    // The general idea is that "Subgraph 3" has a declaration that is kind of useless (it declares entity A
    // that only provides it's own key, so there is never a good reason to use it), but the query planner
    // doesn't know that and will "test" plans including fetch to that subgraphs in its exhaustive search
    // of all options. In theory, the query plan costing mechanism should eliminate such plans in favor of
    // plans not having this inefficient, but an issue in the plan cost computation led to such inefficient
    // to have the same cost as the more efficient one and to be picked (just because it was the first computed).
    // This test ensures this costing bug is fixed.

    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }
    
          type T @key(fields: "idT") {
            idT: ID!
            a: A
          }
    
          type A @key(fields: "idA2") {
            idA2: ID!
          }
          "#,
        Subgraph2: r#"
          type T @key(fields: "idT") {
            idT: ID!
            u: U
          }
    
          type U @key(fields: "idU") {
            idU: ID!
          }
          "#,
        Subgraph3: r#"
          type A @key(fields: "idA1") {
            idA1: ID!
          }
          "#,
        Subgraph4: r#"
          type A @key(fields: "idA1") @key(fields: "idA2") {
            idA1: ID!
            idA2: ID!
          }
          "#,
        Subgraph5: r#"
          type U @key(fields: "idU") {
            idU: ID!
            v: Int
          }
          "#,
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              u {
                v
              }
              a {
                idA1
              }
            }
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                t {
                  __typename
                  idT
                  a {
                    __typename
                    idA2
                  }
                }
              }
            },
            Parallel {
              Sequence {
                Flatten(path: "t") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on T {
                        __typename
                        idT
                      }
                    } =>
                    {
                      ... on T {
                        u {
                          __typename
                          idU
                        }
                      }
                    }
                  },
                },
                Flatten(path: "t.u") {
                  Fetch(service: "Subgraph5") {
                    {
                      ... on U {
                        __typename
                        idU
                      }
                    } =>
                    {
                      ... on U {
                        v
                      }
                    }
                  },
                },
              },
              Flatten(path: "t.a") {
                Fetch(service: "Subgraph4") {
                  {
                    ... on A {
                      __typename
                      idA2
                    }
                  } =>
                  {
                    ... on A {
                      idA1
                    }
                  }
                },
              },
            },
          },
        }
        "#
    );
}
