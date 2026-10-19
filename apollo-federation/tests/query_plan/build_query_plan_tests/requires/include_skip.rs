use apollo_federation::query_plan::FetchDataRewrite;

use crate::query_plan::build_query_plan_support::find_fetch_nodes_for_subgraph;

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
fn it_aliases_multiple_requires_under_a_conditional_root_fragment() {
    // Same subgraphs as `it_handles_multiple_requires_within_the_same_entity_fetch`. Once the
    // `@include` is turned into a condition node, the root fragment is left without a type
    // condition. The conflicting `f` selections inside it must still be aliased, or the
    // Subgraph1 fetch is invalid GraphQL.
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            is: [I!]!
          }
  
          interface I {
            id: ID!
            f: Int
            g: Int
          }
  
          type T1 implements I {
            id: ID!
            f: Int
            g: Int
          }
  
          type T2 implements I @key(fields: "id") {
            id: ID!
            f: Int!
            g: Int @external
          }
  
          type T3 implements I @key(fields: "id") {
            id: ID!
            f: Int
            g: Int @external
          }
        "#,
        Subgraph2: r#"
          type T2 @key(fields: "id") {
            id: ID!
            f: Int! @external
            g: Int @requires(fields: "f")
          }
  
          type T3 @key(fields: "id") {
            id: ID!
            f: Int @external
            g: Int @requires(fields: "f")
          }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          query ($a: Boolean!) {
            ... on Query @include(if: $a) {
              is {
                g
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Include(if: $a) {
            Sequence {
              Fetch(service: "Subgraph1") {
                {
                  ... {
                    is {
                      __typename
                      ... on T1 {
                        g
                      }
                      ... on T2 {
                        __typename
                        id
                        f
                      }
                      ... on T3 {
                        __typename
                        id
                        f__alias_0: f
                      }
                    }
                  }
                }
              },
              Flatten(path: "is.@") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on T2 {
                      __typename
                      id
                      f
                    }
                    ... on T3 {
                      __typename
                      id
                      f
                    }
                  } =>
                  {
                    ... on T2 {
                      g
                    }
                    ... on T3 {
                      g
                    }
                  }
                },
              },
            },
          },
        }
      "###
    );
    // The aliased `f` must be renamed back before it's used as a `@requires` input.
    let fetch_nodes = find_fetch_nodes_for_subgraph("Subgraph1", &plan);
    assert_eq!(fetch_nodes.len(), 1);
    let renames = fetch_nodes[0]
        .output_rewrites
        .iter()
        .map(|rewrite| match rewrite.as_ref() {
            FetchDataRewrite::KeyRenamer(renamer) => format!(
                "{} -> {}",
                renamer
                    .path
                    .iter()
                    .map(|element| element.to_string())
                    .collect::<Vec<_>>()
                    .join("/"),
                renamer.rename_key_to
            ),
            other => panic!("unexpected rewrite {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(renames, ["is/... on T3/f__alias_0 -> f"]);
}
