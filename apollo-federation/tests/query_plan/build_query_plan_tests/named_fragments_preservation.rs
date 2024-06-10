use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

#[test]
fn it_works_with_nested_fragments_1() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            a: Anything
          }
  
          union Anything = A1 | A2 | A3
  
          interface Foo {
            foo: String
            child: Foo
            child2: Foo
          }
  
          type A1 implements Foo {
            foo: String
            child: Foo
            child2: Foo
          }
  
          type A2 implements Foo {
            foo: String
            child: Foo
            child2: Foo
          }
  
          type A3 implements Foo {
            foo: String
            child: Foo
            child2: Foo
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            a {
              ... on A1 {
                ...FooSelect
              }
              ... on A2 {
                ...FooSelect
              }
              ... on A3 {
                ...FooSelect
              }
            }
          }
  
          fragment FooSelect on Foo {
            __typename
            foo
            child {
              ...FooChildSelect
            }
            child2 {
              ...FooChildSelect
            }
          }
  
          fragment FooChildSelect on Foo {
            __typename
            foo
            child {
              child {
                child {
                  foo
                }
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              a {
                __typename
                ... on A1 {
                  ...FooSelect
                }
                ... on A2 {
                  ...FooSelect
                }
                ... on A3 {
                  ...FooSelect
                }
              }
            }

            fragment FooChildSelect on Foo {
              __typename
              foo
              child {
                __typename
                child {
                  __typename
                  child {
                    __typename
                    foo
                  }
                }
              }
            }

            fragment FooSelect on Foo {
              __typename
              foo
              child {
                ...FooChildSelect
              }
              child2 {
                ...FooChildSelect
              }
            }
          },
        }
      "###
    );
}

#[test]
fn it_avoid_fragments_usable_only_once() {
    let planner = planner!(
            Subgraph1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
            v1: V
          }
  
          type V @shareable {
            a: Int
            b: Int
            c: Int
          }
        "#,
            Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v2: V
            v3: V
          }
  
          type V @shareable {
            a: Int
            b: Int
            c: Int
          }
        "#,
    );

    // We use a fragment which does save some on the original query, but as each
    // field gets to a different subgraph, the fragment would only be used one
    // on each sub-fetch and we make sure the fragment is not used in that case.
    assert_plan!(
        &planner,
        r#"
          query {
            t {
              v1 {
                ...OnV
              }
              v2 {
                ...OnV
              }
            }
          }
  
          fragment OnV on V {
            a
            b
            c
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
                  v1 {
                    a
                    b
                    c
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
                  }
                } =>
                {
                  ... on T {
                    v2 {
                      a
                      b
                      c
                    }
                  }
                }
              },
            },
          },
        }
      "###
    );

    // But double-check that if we query 2 fields from the same subgraph, then
    // the fragment gets used now.
    assert_plan!(
        &planner,
        r#"
          query {
            t {
              v2 {
                ...OnV
              }
              v3 {
                ...OnV
              }
            }
          }
  
          fragment OnV on V {
            a
            b
            c
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
            }
          }
        },
        Flatten(path: "t") {
          Fetch(service: "Subgraph2") {
            {
              ... on T {
                __typename
                id
              }
            } =>
            {
              ... on T {
                v2 {
                  ...OnV
                }
                v3 {
                  ...OnV
                }
              }
            }

            fragment OnV on V {
              a
              b
              c
            }
          },
        },
      },
    }
    "###
    );
}

mod respects_query_planner_option_reuse_query_fragments {
    use super::*;

    const SUBGRAPH1: &str = r#"
            type Query {
              t: T
            }
  
            type T {
              a1: A
              a2: A
            }
  
            type A {
              x: Int
              y: Int
            }
    "#;
    const QUERY: &str = r#"
            query {
              t {
                a1 {
                  ...Selection
                }
                a2 {
                  ...Selection
                }
              }
            }

            fragment Selection on A {
              x
              y
            }
    "#;

    #[test]
    fn respects_query_planner_option_reuse_query_fragments_true() {
        let reuse_query_fragments = true;
        let planner = planner!(
          config = QueryPlannerConfig {reuse_query_fragments, ..Default::default()},
          Subgraph1: SUBGRAPH1,
        );
        let query = QUERY;

        assert_plan!(
            &planner,
            query,
            @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                a1 {
                  ...Selection
                }
                a2 {
                  ...Selection
                }
              }
            }

            fragment Selection on A {
              x
              y
            }
          },
        }
        "###
        );
    }

    #[test]
    fn respects_query_planner_option_reuse_query_fragments_false() {
        let reuse_query_fragments = false;
        let planner = planner!(
          config = QueryPlannerConfig {reuse_query_fragments, ..Default::default()},
          Subgraph1: SUBGRAPH1,
        );
        let query = QUERY;

        assert_plan!(
            &planner,
            query,
            @r#"
            QueryPlan {
              Fetch(service: "Subgraph1") {
                {
                  t {
                    a1 {
                      x
                      y
                    }
                    a2 {
                      x
                      y
                    }
                  }
                }
              },
            }
            "#
        );
    }
}

#[test]
fn it_works_with_nested_fragments_when_only_the_nested_fragment_gets_preserved() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: V
            b: V
          }

          type V {
            v1: Int
            v2: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              ...OnT
            }
          }

          fragment OnT on T {
            a {
              ...OnV
            }
            b {
              ...OnV
            }
          }

          fragment OnV on V {
            v1
            v2
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                a {
                  ...OnV
                }
                b {
                  ...OnV
                }
              }
            }

            fragment OnV on V {
              v1
              v2
            }
          },
        }
      "###
    );
}

#[test]
#[should_panic(
    expected = r#"variable `$if` of type `Boolean` cannot be used for argument `if` of type `Boolean!`"#
)]
// TODO: investigate this failure
fn it_preserves_directives_when_fragment_not_used() {
    // (because used only once)
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: Int
            b: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query test($if: Boolean) {
            t {
              id
              ...OnT @include(if: $if)
            }
          }

          fragment OnT on T {
            a
            b
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                id
                ... on T @include(if: $if) {
                  a
                  b
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
#[should_panic(
    expected = "variable `$test1` of type `Boolean` cannot be used for argument `if` of type `Boolean!`"
)]
// TODO: investigate this failure
fn it_preserves_directives_when_fragment_is_reused() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: Int
            b: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query test($test1: Boolean, $test2: Boolean) {
            t {
              id
              ...OnT @include(if: $test1)
              ...OnT @include(if: $test2)
            }
          }

          fragment OnT on T {
            a
            b
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                id
                ...OnT @include(if: $test1)
                ...OnT @include(if: $test2)
              }
            }

            fragment OnT on T {
              a
              b
            }
          },
        }
      "###
    );
}

#[test]
fn it_does_not_try_to_apply_fragments_that_are_not_valid_for_the_subgaph() {
    // Slightly artificial example for simplicity, but this highlight the problem.
    // In that example, the only queried subgraph is the first one (there is in fact
    // no way to ever reach the 2nd one), so the plan should mostly simply forward
    // the query to the 1st subgraph, but a subtlety is that the named fragment used
    // in the query is *not* valid for Subgraph1, because it queries `b` on `I`, but
    // there is no `I.b` in Subgraph1.
    // So including the named fragment in the fetch would be erroneous: the subgraph
    // server would reject it when validating the query, and we must make sure it
    // is not reused.
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i1: I
            i2: I
          }

          interface I {
            a: Int
          }

          type T implements I {
            a: Int
            b: Int
          }
        "#,
        Subgraph2: r#"
          interface I {
            a: Int
            b: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            i1 {
              ... on T {
                ...Frag
              }
            }
            i2 {
              ... on T {
                ...Frag
              }
            }
          }

          fragment Frag on I {
            b
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              i1 {
                __typename
                ... on T {
                  b
                }
              }
              i2 {
                __typename
                ... on T {
                  b
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn it_handles_fragment_rebasing_in_a_subgraph_where_some_subtyping_relation_differs() {
    // This test is designed such that type `Outer` implements the interface `I` in `Subgraph1`
    // but not in `Subgraph2`, yet `I` exists in `Subgraph2` (but only `Inner` implements it
    // there). Further, the operations we test have a fragment on I (`IFrag` below) that is
    // used "in the context of `Outer`" (at the top-level of fragment `OuterFrag`).
    //
    // What this all means is that `IFrag` can be rebased in `Subgraph2` "as is" because `I`
    // exists there with all its fields, but as we rebase `OuterFrag` on `Subgraph2`, we
    // cannot use `...IFrag` inside it (at the top-level), because `I` and `Outer` do
    // no intersect in `Subgraph2` and this would be an invalid selection.
    //
    // Previous versions of the code were not handling this case and were error out by
    // creating the invalid selection (#2721), and this test ensures this is fixed.
    let planner = planner!(
        Subgraph1: r#"
          type V @shareable {
            x: Int
          }

          interface I {
            v: V
          }

          type Outer implements I @key(fields: "id") {
            id: ID!
            v: V
          }
        "#,
        Subgraph2: r#"
          type Query {
            outer1: Outer
            outer2: Outer
          }

          type V @shareable {
            x: Int
          }

          interface I {
            v: V
            w: Int
          }

          type Inner implements I {
            v: V
            w: Int
          }

          type Outer @key(fields: "id") {
            id: ID!
            inner: Inner
            w: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...IFrag
            inner {
              ...IFrag
            }
          }

          fragment IFrag on I {
            v {
              x
            }
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  ...OuterFrag
                  id
                }
                outer2 {
                  __typename
                  ...OuterFrag
                  id
                }
              }

              fragment OuterFrag on Outer {
                inner {
                  v {
                    x
                  }
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
                    }
                  }
                },
              },
            },
          },
        }
        "#
    );

    // We very slighly modify the operation to add an artificial indirection within `IFrag`.
    // This does not really change the query, and should result in the same plan, but
    // ensure the code handle correctly such indirection.
    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...IFrag
            inner {
              ...IFrag
            }
          }

          fragment IFrag on I {
            ...IFragDelegate
          }

          fragment IFragDelegate on I {
            v {
              x
            }
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  ...OuterFrag
                  id
                }
                outer2 {
                  __typename
                  ...OuterFrag
                  id
                }
              }

              fragment OuterFrag on Outer {
                inner {
                  v {
                    x
                  }
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
                    }
                  }
                },
              },
            },
          },
        }
        "#
    );

    // The previous cases tests the cases where nothing in the `...IFrag` spread at the
    // top-level of `OuterFrag` applied at all: it all gets eliminated in the plan. But
    // in the schema of `Subgraph2`, while `Outer` does not implement `I` (and does not
    // have `v` in particular), it does contains field `w` that `I` also have, so we
    // add that field to `IFrag` and make sure we still correctly query that field.

    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...IFrag
            inner {
              ...IFrag
            }
          }

          fragment IFrag on I {
            v {
              x
            }
            w
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  ...OuterFrag
                  id
                }
                outer2 {
                  __typename
                  ...OuterFrag
                  id
                }
              }

              fragment OuterFrag on Outer {
                w
                inner {
                  v {
                    x
                  }
                  w
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
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

#[test]
fn it_handles_fragment_rebasing_in_a_subgraph_where_some_union_membership_relation_differs() {
    // This test is similar to the subtyping case (it tests the same problems), but test the case
    // of unions instead of interfaces.
    let planner = planner!(
        Subgraph1: r#"
          type V @shareable {
            x: Int
          }

          union U = Outer

          type Outer @key(fields: "id") {
            id: ID!
            v: Int
          }
        "#,
        Subgraph2: r#"
          type Query {
            outer1: Outer
            outer2: Outer
          }

          union U = Inner

          type Inner {
            v: Int
            w: Int
          }

          type Outer @key(fields: "id") {
            id: ID!
            inner: Inner
            w: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...UFrag
            inner {
              ...UFrag
            }
          }

          fragment UFrag on U {
            ... on Outer {
              v
            }
            ... on Inner {
              v
            }
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  ...OuterFrag
                  id
                }
                outer2 {
                  __typename
                  ...OuterFrag
                  id
                }
              }

              fragment OuterFrag on Outer {
                inner {
                  v
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v
                    }
                  }
                },
              },
            },
          },
        }
        "#
    );

    // We very slighly modify the operation to add an artificial indirection within `IFrag`.
    // This does not really change the query, and should result in the same plan, but
    // ensure the code handle correctly such indirection.
    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...UFrag
            inner {
              ...UFrag
            }
          }

          fragment UFrag on U {
            ...UFragDelegate
          }

          fragment UFragDelegate on U {
            ... on Outer {
              v
            }
            ... on Inner {
              v
            }
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  ...OuterFrag
                  id
                }
                outer2 {
                  __typename
                  ...OuterFrag
                  id
                }
              }

              fragment OuterFrag on Outer {
                inner {
                  v
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v
                    }
                  }
                },
              },
            },
          },
        }
        "#
    );

    // The previous cases tests the cases where nothing in the `...IFrag` spread at the
    // top-level of `OuterFrag` applied at all: it all gets eliminated in the plan. But
    // in the schema of `Subgraph2`, while `Outer` does not implement `I` (and does not
    // have `v` in particular), it does contains field `w` that `I` also have, so we
    // add that field to `IFrag` and make sure we still correctly query that field.
    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...UFrag
            inner {
              ...UFrag
            }
          }

          fragment UFrag on U {
            ... on Outer {
              v
              w
            }
            ... on Inner {
              v
            }
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  ...OuterFrag
                  id
                }
                outer2 {
                  __typename
                  ...OuterFrag
                  id
                }
              }

              fragment OuterFrag on Outer {
                w
                inner {
                  v
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v
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
