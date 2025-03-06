use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

fn generate_fragments_config() -> QueryPlannerConfig {
    QueryPlannerConfig {
        generate_query_fragments: true,
        ..Default::default()
    }
}

const SUBGRAPH: &str = r#"
      directive @custom on INLINE_FRAGMENT | FRAGMENT_SPREAD

      type Query {
        t: T
        t2: T
      }

      union T = A | B

      type A {
        x: Int
        y: Int
        z: Int
        t: T
      }

      type B {
        z: Int
      }
"#;

// TODO this test shows a worse plan than reused fragments when generated fragments
// target concrete types whereas hand-crafted ones reference abstract types
#[test]
fn it_handles_nested_fragment_generation_from_operation_with_fragments() {
    let planner = planner!(
        config = generate_fragments_config(),
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
    let operation = r#"
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
    "#;
    assert_plan!(
        &planner,
        operation,

        // This is a test case that shows worse result
        // QueryPlan {
        //           Fetch(service: "Subgraph1") {
        //             {
        //               a {
        //                 __typename
        //                 ... on A1 {
        //                   ...FooSelect
        //                 }
        //                 ... on A2 {
        //                   ...FooSelect
        //                 }
        //                 ... on A3 {
        //                   ...FooSelect
        //                 }
        //               }
        //             }
        //
        //             fragment FooChildSelect on Foo {
        //               __typename
        //               foo
        //               child {
        //                 __typename
        //                 child {
        //                   __typename
        //                   child {
        //                     __typename
        //                     foo
        //                   }
        //                 }
        //               }
        //             }
        //
        //             fragment FooSelect on Foo {
        //               __typename
        //               foo
        //               child {
        //                 ...FooChildSelect
        //               }
        //               child2 {
        //                 ...FooChildSelect
        //               }
        //             }
        //           },
        //         }
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          a {
            __typename
            ... on A1 {
              __typename
              foo
              child {
                ...d
              }
              child2 {
                ...d
              }
            }
            ... on A2 {
              __typename
              foo
              child {
                ...d
              }
              child2 {
                ...d
              }
            }
            ... on A3 {
              __typename
              foo
              child {
                ...d
              }
              child2 {
                ...d
              }
            }
          }
        }

        fragment a on Foo {
          __typename
          foo
        }

        fragment b on Foo {
          __typename
          child {
            ...a
          }
        }

        fragment c on Foo {
          __typename
          child {
            ...b
          }
        }

        fragment d on Foo {
          __typename
          foo
          child {
            ...c
          }
        }
      },
    }
    "###
    );
}

#[test]
fn it_migrates_skip_include() {
    let planner = planner!(
        config = generate_fragments_config(),
        Subgraph1: SUBGRAPH,
    );
    assert_plan!(
        &planner,
        r#"
        query ($var: Boolean!) {
          t {
            ... on A {
              x
              y
              t {
                ... on A @include(if: $var) {
                  x
                  y
                }
                ... on A @skip(if: $var) {
                  x
                  y
                }
                ... on A @custom {
                  x
                  y
                }
              }
            }
          }
        }
        "#,

        // Note: `... on A @custom {}` won't be replaced, since it has a custom directive. Even
        // though it also supports being used on a named fragment spread, we cannot assume that
        // the behaviour is exactly the same. We will replace its subselection though.
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          t {
            __typename
            ... on A {
              x
              y
              t {
                __typename
                ...a @include(if: $var)
                ...a @skip(if: $var)
                ... on A @custom {
                  ...a
                }
              }
            }
          }
        }

        fragment a on A {
          x
          y
        }
      },
    }
    "###
    );
}

#[test]
fn it_identifies_and_reuses_equivalent_fragments_that_arent_identical() {
    let planner = planner!(
        config = generate_fragments_config(),
        Subgraph1: SUBGRAPH,
    );
    assert_plan!(
        &planner,
        r#"
        query {
          t {
            ... on A {
              x
              y
            }
          }
          t2 {
            ... on A {
              y
              x
            }
          }
        }
      "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          t {
            ...b
          }
          t2 {
            ...b
          }
        }

        fragment a on A {
          x
          y
        }

        fragment b on T {
          __typename
          ...a
        }
      },
    }
    "###
    );
}

#[test]
fn same_as_js_router798() {
    let planner = planner!(
        config = generate_fragments_config(),
        Subgraph1: r#"
            interface Interface { a: Int }
            type Y implements Interface { a: Int b: Int }
            type Z implements Interface { a: Int c: Int }

            type Query {
                interfaces(id: Int!): Interface
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query($var0: Boolean! = true) {
              ... @skip(if: $var0) {
                field0: interfaces(id: 0) {
                  field1: __typename
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Skip(if: $var0) {
        Fetch(service: "Subgraph1") {
          {
            ... {
              field0: interfaces(id: 0) {
                __typename
                field1: __typename
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
fn works_with_key_chains() {
    let planner = planner!(
        config = generate_fragments_config(),
        Subgraph1: r#"
      type Query {
        t: T
      }

      type T @key(fields: "id1") {
        id1: ID!
      }
      "#,
        Subgraph2: r#"
      type T @key(fields: "id1") @key(fields: "id2") {
        id1: ID!
        id2: ID!
        u1: U
        u2: U
      }

      type U {
        a: String
        b: Int
      }
      "#,
        Subgraph3: r#"
      type T @key(fields: "id2") {
        id2: ID!
        x: Int
        y: Int
      }
      "#
    );

    // Note: querying `id2` is only purpose, because there is 2 choice to get `id2` (either
    // from then 2nd or 3rd subgraph), and that create some choice in the query planning algorithm,
    // so excercices additional paths.
    assert_plan!(
      &planner,
      r#"
      {
        t {
          id2
          x
          y
          u1 {
            a
            b
          }
          u2 {
            a
            b
          }
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
              id1
            }
          }
        },
        Flatten(path: "t") {
          Fetch(service: "Subgraph2") {
            {
              ... on T {
                __typename
                id1
              }
            } =>
            {
              ... on T {
                id2
                u1 {
                  ...a
                }
                u2 {
                  ...a
                }
              }
            }

            fragment a on U {
              a
              b
            }
          },
        },
        Flatten(path: "t") {
          Fetch(service: "Subgraph3") {
            {
              ... on T {
                __typename
                id2
              }
            } =>
            {
              ... on T {
                x
                y
              }
            }
          },
        },
      },
    }
  "###
    );
}

// TODO this test shows redundant inline fragment in the "normalized" query
// - ... on T2 inline fragment should be dropped during normalization
#[test]
fn another_mix_of_fragments_indirection_and_unions() {
    // This tests that the issue reported on https://github.com/apollographql/router/issues/3172 is resolved.
    let planner = planner!(
        config = generate_fragments_config(),
        Subgraph1: r#"
          type Query {
            owner: Owner!
          }

          interface OItf {
            id: ID!
            v0: String!
          }

          type Owner implements OItf {
            id: ID!
            v0: String!
            u: [U]
          }

          union U = T1 | T2

          interface I {
            id1: ID!
            id2: ID!
          }

          type T1 implements I {
            id1: ID!
            id2: ID!
            owner: Owner!
          }

          type T2 implements I {
            id1: ID!
            id2: ID!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            owner {
              u {
                ... on I {
                  id1
                  id2
                }
                ...Fragment1
                ...Fragment2
              }
            }
          }

          fragment Fragment1 on T1 {
            owner {
              ... on Owner {
                ...Fragment3
              }
            }
          }

          fragment Fragment2 on T2 {
            ...Fragment4
            id1
          }

          fragment Fragment3 on OItf {
            v0
          }

          fragment Fragment4 on I {
            id1
            id2
            __typename
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              owner {
                u {
                  __typename
                  ... on I {
                    __typename
                    id1
                    id2
                  }
                  ... on T1 {
                    owner {
                      v0
                    }
                  }
                  ... on T2 {
                    __typename
                    id1
                    id2
                  }
                }
              }
            }
          },
        }
      "###
    );

    assert_plan!(
        &planner,
        r#"
          {
            owner {
              u {
                ... on I {
                  id1
                  id2
                }
                ...Fragment1
                ...Fragment2
              }
            }
          }

          fragment Fragment1 on T1 {
            owner {
              ... on Owner {
                ...Fragment3
              }
            }
          }

          fragment Fragment2 on T2 {
            ...Fragment4
            id1
          }

          fragment Fragment3 on OItf {
            v0
          }

          fragment Fragment4 on I {
            id1
            id2
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              owner {
                u {
                  __typename
                  ... on I {
                    __typename
                    id1
                    id2
                  }
                  ... on T1 {
                    owner {
                      v0
                    }
                  }
                  ... on T2 {
                    id1
                    id2
                  }
                }
              }
            }
          },
        }
      "###
    );
}
