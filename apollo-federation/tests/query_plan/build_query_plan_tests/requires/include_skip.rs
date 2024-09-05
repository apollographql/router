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
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure (redundant inline spread)
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
                  }
                },
              }
            },
          }
        "###
    );
}

#[test]
fn todo_give_name_one() {
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
}

#[test]
fn todo_give_name_two() {
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
                  bar {
                    ... on Bar @include(if: $test) {
                      a
                    }
                  }
                }
              }
            }
          "#,
        @r###"
        QueryPlan {
          Include(if: $test) {
            Sequence {
              Fetch(service: "Subgraph1") {
                {
                  foo {
                    ... on Foo {
                      id
                      bar {
                        ... on Bar {
                          __typename
                          id
                        }
                      }
                    }
                  }
                }
              },
              Flatten(path: "foo.bar") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on Bar {
                      ... on Bar {
                        ... on Bar {
                          __typename
                          id
                        }
                      }
                    }
                  } =>
                  {
                    ... on Bar {
                      ... on Bar {
                        ... on Bar {
                          a
                        }
                      }
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
fn todo_give_name_three() {
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
