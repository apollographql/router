#[test]
fn it_preservers_aliased_typename() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            t {
              foo: __typename
              x
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                foo: __typename
                x
              }
            }
          },
        }
      "###
    );

    assert_plan!(
        &planner,
        r#"
          query {
            t {
              foo: __typename
              x
              __typename
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                __typename
                foo: __typename
                x
              }
            }
          },
        }
      "###
    );
}

#[test]
fn it_does_not_needlessly_consider_options_for_typename() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            s: S
          }
  
          type S @key(fields: "id") {
            id: ID
          }
        "#,
        Subgraph2: r#"
          type S @key(fields: "id") {
            id: ID
            t: T @shareable
          }
  
          type T {
            x: Int
          }
        "#,
        Subgraph3: r#"
          type S @key(fields: "id") {
            id: ID
            t: T @shareable
          }
  
          type T {
            id: ID!
            y: Int
          }
        "#
    );

    // This tests the patch from https://github.com/apollographql/federation/pull/2137.
    // Namely, the schema is such that `x` can only be fetched from one subgraph, but
    // technically __typename can be fetched from 2 subgraphs. However, the optimization
    // we test for is that we actually don't consider both choices for __typename and
    // instead only evaluate a single query plan (the assertion on `evaluatePlanCount`)
    let plan = assert_plan!(
        &planner,
        r#"
          query {
            s {
              t {
                __typename
                x
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                s {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "s") {
              Fetch(service: "Subgraph2") {
                {
                  ... on S {
                    __typename
                    id
                  }
                } =>
                {
                  ... on S {
                    t {
                      __typename
                      x
                    }
                  }
                }
              },
            },
          },
        }
      "###
    );
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 1);

    // Almost the same test, but we artificially create a case where the result set
    // for `s` has a __typename alongside just an inline fragments. This should
    // change nothing to the example (the __typename on `s` is trivially fetched
    // from the 1st subgraph and does not create new choices), but an early bug
    // in the implementation made this example forgo the optimization of the
    // __typename within `t`. We make sure this is not case (that we still only
    // consider a single choice of plan).
    let plan = assert_plan!(
        &planner,
        r#"
          query {
            s {
              __typename
              ... on S {
                t {
                  __typename
                  x
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
                s {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "s") {
              Fetch(service: "Subgraph2") {
                {
                  ... on S {
                    __typename
                    id
                  }
                } =>
                {
                  ... on S {
                    __typename
                    t {
                      __typename
                      x
                    }
                  }
                }
              },
            },
          },
        }
      "###
    );
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 1);
}

#[test]
fn add_back_sibling_typename_to_interface_object() {
    let planner = planner!(
        Subgraph1: r#"
            interface Item @key(fields: "id") {
                id: ID!
                name: String!
            }

            type Book implements Item @key(fields: "id") {
                id: ID!
                name: String!
            }

            type Audio implements Item @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#,
        Subgraph2: r#"
            type Query {
                currentItem: Item
            }

            type Item @key(fields: "id") @interfaceObject {
                id: ID!
            }
        "#,
    );
    // One of the inline fragments have `__typename`, which is removed at first and
    // added back in the fetch query. We need to make sure that does not cause rebasing
    // error (such as FED-251).
    assert_plan!(
        &planner,
        r#"
            {
            currentItem {
                ... on Book {
                    __typename
                    id
                    name
                }
                ... on Audio {
                    id
                    name
                }
            }
            }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph2") {
          {
            currentItem {
              __typename
              id
            }
          }
        },
        Flatten(path: "currentItem") {
          Fetch(service: "Subgraph1") {
            {
              ... on Item {
                __typename
                id
              }
            } =>
            {
              ... on Item {
                __typename
                ... on Book {
                  name
                }
                ... on Audio {
                  name
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

#[test]
fn test_indirect_branch_merging_with_typename_sibling() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
                test: T
            }

            interface T {
                id: ID!
            }

            type A implements T @key(fields: "id") {
                id: ID!
            }

            type B implements T @key(fields: "id") {
                id: ID!
            }
        "#,
        Subgraph2: r#"
            interface T {
                id: ID!
                f: Int!
            }

            type A implements T @key(fields: "id") {
                id: ID!
                f: Int!
            }

            type B implements T @key(fields: "id") {
                id: ID!
                f: Int!
            }
        "#,
    );
    // This operation has two `f` selection instances: One with __typename sibling and one without.
    // It creates multiple identical branches in the form of `... on A { f }` with different `f`.
    // The query plan must chose one over the other, which is implementation specific.
    // Currently, the last one is chosen.
    assert_plan!(
        &planner,
        r#"
            {
                test {
                    __typename
                    f # <= This will have a sibling typename value.
                    ... on A {
                        f # <= This one will have no sibling typename.
                    }
                }
            }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            test {
              __typename
              ... on A {
                __typename
                id
              }
              ... on B {
                __typename
                id
              }
            }
          }
        },
        Flatten(path: "test") {
          Fetch(service: "Subgraph2") {
            {
              ... on A {
                __typename
                id
              }
              ... on B {
                __typename
                id
              }
            } =>
            {
              ... on A {
                f
              }
              ... on B {
                __typename
                f
              }
            }
          },
        },
      },
    }
    "###
    );
}
