#[test]
fn it_handles_a_simple_at_requires_triggered_within_a_conditional() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              t: T
            }
  
            type T @key(fields: "id") {
              id: ID!
              a: Int
            }
        "#,
        Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              a: Int @external
              b: Int @requires(fields: "a")
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query foo($test: Boolean!) {
              t @include(if: $test) {
                b
              }
            }
          "#,
        @r###"
          QueryPlan {
            Include(if: $test) {
              Sequence {
                Fetch(service: "Subgraph1") {
                  {
                    t {
                      __typename
                      id
                      a
                    }
                  }
                },
                Flatten(path: "t") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on T {
                        __typename
                        id
                        a
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
            },
          }
        "###
    );
}

#[test]
fn it_handles_an_at_requires_triggered_conditionally() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              t: T
            }
  
            type T @key(fields: "id") {
              id: ID!
              a: Int
            }
        "#,
        Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              a: Int @external
              b: Int @requires(fields: "a")
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query foo($test: Boolean!) {
              t {
                b @include(if: $test)
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
                    ... on T @include(if: $test) {
                      a
                    }
                  }
                }
              },
              Include(if: $test) {
                Flatten(path: "t") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on T {
                        __typename
                        id
                        a
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
            },
          }
        "###
    );
}

#[test]
fn it_handles_an_at_requires_where_multiple_conditional_are_involved() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              a: A
            }
  
            type A @key(fields: "idA") {
              idA: ID!
            }
        "#,
        Subgraph2: r#"
            type A @key(fields: "idA") {
              idA: ID!
              b: [B]
            }
  
            type B @key(fields: "idB") {
              idB: ID!
              required: Int
            }
        "#,
        Subgraph3: r#"
            type B @key(fields: "idB") {
              idB: ID!
              c: Int @requires(fields: "required")
              required: Int @external
            }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
            query foo($test1: Boolean!, $test2: Boolean!) {
              a @include(if: $test1) {
                b @include(if: $test2) {
                  c
                }
              }
            }
          "#,
        @r###"
          QueryPlan {
            Include(if: $test1) {
              Sequence {
                Fetch(service: "Subgraph1") {
                  {
                    a {
                      __typename
                      idA
                    }
                  }
                },
                Include(if: $test2) {
                  Sequence {
                    Flatten(path: "a") {
                      Fetch(service: "Subgraph2") {
                        {
                          ... on A {
                            __typename
                            idA
                          }
                        } =>
                        {
                          ... on A {
                            b {
                              __typename
                              idB
                              required
                            }
                          }
                        }
                      },
                    },
                    Flatten(path: "a.b.@") {
                      Fetch(service: "Subgraph3") {
                        {
                          ... on B {
                            ... on B {
                              __typename
                              idB
                              required
                            }
                          }
                        } =>
                        {
                          ... on B {
                            ... on B {
                              c
                            }
                          }
                        }
                      },
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
fn unnecessary_include_is_stripped_from_fragments() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              foo: Foo,
            }
            type Foo @key(fields: "id") {
              id: ID,
              bar: Bar,
            }
            type Bar @key(fields: "id") {
              id: ID,
            }
        "#,
        Subgraph2: r#"
            type Bar @key(fields: "id") {
              id: ID,
              a: Int,
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        query foo($test: Boolean!) {
          foo @include(if: $test) {
            ... on Foo @include(if: $test) {
              id
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Include(if: $test) {
            Fetch(service: "Subgraph1") {
              {
                foo {
                  ... on Foo {
                    id
                  }
                }
              }
            },
          },
        }
        "###
    );
    assert_plan!(
        &planner,
        r#"
        query foo($test: Boolean!) {
          foo @include(if: $test) {
            ... on Foo @include(if: $test) {
              id
              bar {
                ... on Bar @include(if: $test) {
                  id
                }
              }
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Include(if: $test) {
            Fetch(service: "Subgraph1") {
              {
                foo {
                  ... on Foo {
                    id
                    bar {
                      ... on Bar {
                        id
                      }
                    }
                  }
                }
              }
            },
          },
        }
        "###
    );
}

#[test]
fn selections_are_not_overwritten_after_removing_directives() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              foo: Foo,
            }
            type Foo @key(fields: "id") {
              id: ID,
              foo: Foo,
              bar: Bar,
            }
            type Bar @key(fields: "id") {
              id: ID,
            }
        "#,
        Subgraph2: r#"
            type Bar @key(fields: "id") {
              id: ID,
              a: Int,
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query foo($test: Boolean!) {
            foo @include(if: $test) {
              ... on Foo {
                id
                foo {
                  ... on Foo @include(if: $test) {
                    bar {
                      id
                    }
                  }
                }
              }
            }
          }
          "#,
        @r###"
        QueryPlan {
          Include(if: $test) {
            Fetch(service: "Subgraph1") {
              {
                foo {
                  id
                  foo {
                    ... on Foo {
                      bar {
                        id
                      }
                    }
                  }
                }
              }
            },
          },
        }
        "###
    );
}

#[test]
fn sibling_requires_not_gated_by_sibling_include() {
    // Two sibling fields on Container both @requires(fields: "externalId"), but only one
    // is conditional. Since `conditionalField` is processed first, its @requires resolution
    // for `externalId` is cached with the @include context. Without the fix, `items` gets
    // a cache hit with this context-polluted resolution and `externalId` is wrongly gated
    // behind @include.
    //
    // Expected: `externalId` is fetched unconditionally (needed for `items`).
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              container: Container
            }
            type Container @key(fields: "id") {
              id: ID!
              externalId: ID!
            }
        "#,
        Subgraph2: r#"
            type Container @key(fields: "id") {
              id: ID!
              externalId: ID! @external
              conditionalField: String @requires(fields: "externalId")
              items: [Item] @requires(fields: "externalId")
            }
            type Item {
              id: ID!
              quantity: Int!
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query Test($cond: Boolean!) {
              container {
                conditionalField @include(if: $cond)
                items {
                  id
                  quantity
                }
              }
            }
        "#,
        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "Subgraph1") {
                {
                  container {
                    __typename
                    id
                    externalId
                    ... on Container @include(if: $cond) {
                      externalId
                    }
                  }
                }
              },
              Flatten(path: "container") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on Container {
                      __typename
                      id
                      externalId
                    }
                  } =>
                  {
                    ... on Container {
                      conditionalField @include(if: $cond)
                      items {
                        id
                        quantity
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
fn requires_input_not_gated_by_unrelated_include() {
    // Two root fields return the same entity type — one conditional, one unconditional.
    // The entity field `state` has @requires(fields: "locationId") which lives in a
    // separate subgraph, forcing cross-subgraph key resolution. Since `conditionalLevel`
    // is processed first, its resolution is cached with the @include context. Without the
    // fix, `directLevel`'s resolution gets a cache hit with this context-polluted result
    // and `locationId` is wrongly gated behind @include.
    //
    // Expected: `directLevel`'s fetch to SubgraphB fetches `locationId` unconditionally.
    // The entire `conditionalLevel` path is inside Include(if: $cond).
    let planner = planner!(
        SubgraphA: r#"
            type Query {
              directLevel: Level
              conditionalLevel: Level
            }
            type Level @key(fields: "id") {
              id: ID!
            }
        "#,
        SubgraphB: r#"
            type Level @key(fields: "id") {
              id: ID!
              locationId: ID!
            }
        "#,
        SubgraphC: r#"
            type Level @key(fields: "id") {
              id: ID!
              locationId: ID! @external
              state: String! @requires(fields: "locationId")
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query Test($cond: Boolean!) {
              conditionalLevel @include(if: $cond) {
                state
              }
              directLevel {
                state
              }
            }
        "#,
        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "SubgraphA") {
                {
                  conditionalLevel @include(if: $cond) {
                    __typename
                    id
                  }
                  directLevel {
                    __typename
                    id
                  }
                }
              },
              Parallel {
                Sequence {
                  Flatten(path: "directLevel") {
                    Fetch(service: "SubgraphB") {
                      {
                        ... on Level {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on Level {
                          locationId
                        }
                      }
                    },
                  },
                  Flatten(path: "directLevel") {
                    Fetch(service: "SubgraphC") {
                      {
                        ... on Level {
                          __typename
                          locationId
                          id
                        }
                      } =>
                      {
                        ... on Level {
                          state
                        }
                      }
                    },
                  },
                },
                Include(if: $cond) {
                  Sequence {
                    Flatten(path: "conditionalLevel") {
                      Fetch(service: "SubgraphB") {
                        {
                          ... on Level {
                            __typename
                            id
                          }
                        } =>
                        {
                          ... on Level {
                            locationId
                          }
                        }
                      },
                    },
                    Flatten(path: "conditionalLevel") {
                      Fetch(service: "SubgraphC") {
                        {
                          ... on Level {
                            __typename
                            locationId
                            id
                          }
                        } =>
                        {
                          ... on Level {
                            state
                          }
                        }
                      },
                    },
                  },
                },
              },
            },
          }
        "###
    );
}
