/// Regression coverage for static skip directives interacting with `@requires`.
///
/// `b @skip(if: true)` should statically drop `b`; the only live reason to
/// visit Subgraph2 is the sibling `c`, which has no `@requires`. Therefore
/// the Subgraph1 fetch should not pull `a`, and the Subgraph2 entity fetch's
/// declared inputs should match Subgraph2's `@key` for `T` (just `id`) — not
/// include `a`. Surfaced by fuzzing with `FUZZ_CORRECTNESS=1`.
///
/// TODO(FED-707): the planner currently leaks the skipped `b`'s
/// `@requires(fields: "a")` into the Subgraph2 entity fetch input set,
/// which is then not matched by Subgraph2's `@key` (just `id`) nor by any
/// active `@requires`. `correctness::check_plan` flags the mismatch, so
/// this test demonstrates the failure via `#[should_panic]`. Once the
/// planner is fixed, remove `#[should_panic]` and update the snapshot.
#[test]
#[should_panic(expected = "generated correct plan")]
fn handles_static_skip_on_a_requires_field() {
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
              c: Int
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            {
              t {
                b @skip(if: true)
                c
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
              ... on T @skip(if: true) {
                a
              }
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
                b @skip(if: true)
                c
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
