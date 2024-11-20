use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

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

#[test]
fn it_respects_generate_query_fragments_option() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
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
            ... on B {
              z
            }
          }
        }
        "#,



      // Note: `... on B {}` won't be replaced, since it has only one field.
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          t {
            __typename
            ..._generated_onA2_0
            ... on B {
              z
            }
          }
        }

        fragment _generated_onA2_0 on A {
          x
          y
        }
      },
    }
    "###
    );
}

#[test]
fn it_handles_nested_fragment_generation() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
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
              t {
                ... on A {
                  x
                  y
                }
                ... on B {
                  z
                }
              }
            }
          }
        }
        "#,

        // Note: `... on B {}` won't be replaced, since it has only one field.
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          t {
            __typename
            ..._generated_onA3_0
          }
        }

        fragment _generated_onA2_0 on A {
          x
          y
        }

        fragment _generated_onA3_0 on A {
          x
          y
          t {
            __typename
            ..._generated_onA2_0
            ... on B {
              z
            }
          }
        }
      },
    }
    "###
    );
}

#[test]
fn it_handles_fragments_with_one_non_leaf_field() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
        Subgraph1: SUBGRAPH,
    );

    assert_plan!(
        &planner,
        r#"
        query {
          t {
            ... on A {
              t {
                ... on B {
                  z
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
          t {
            __typename
            ..._generated_onA1_0
          }
        }

        fragment _generated_onA1_0 on A {
          t {
            __typename
            ... on B {
              z
            }
          }
        }
      },
    }
    "###
    );
}

/// XXX(@goto-bus-stop): this test is meant to check that fragments with @skip and @include *are*
/// migrated. But we are currently matching JS behavior, where they are not. This test should be
/// updated when we remove JS compatibility.
#[test]
fn it_migrates_skip_include() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
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
        // the behaviour is exactly the same.
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          t {
            __typename
            ..._generated_onA3_0
          }
        }

        fragment _generated_onA3_0 on A {
          x
          y
          t {
            __typename
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
      },
    }
    "###
    );
}
#[test]
fn it_identifies_and_reuses_equivalent_fragments_that_arent_identical() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
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
            __typename
            ..._generated_onA2_0
          }
          t2 {
            __typename
            ..._generated_onA2_0
          }
        }

        fragment _generated_onA2_0 on A {
          x
          y
        }
      },
    }
    "###
    );
}

#[test]
fn fragments_that_share_a_hash_but_are_not_identical_generate_their_own_fragment_definitions() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
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
              z
            }
          }
        }
      "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          t {
            __typename
            ..._generated_onA2_0
          }
          t2 {
            __typename
            ..._generated_onA2_1
          }
        }

        fragment _generated_onA2_0 on A {
          x
          y
        }

        fragment _generated_onA2_1 on A {
          y
          z
        }
      },
    }
    "###
    );
}

#[test]
fn same_as_js_router798() {
    let planner = planner!(
        config = QueryPlannerConfig { generate_query_fragments: true, reuse_query_fragments: false, ..Default::default() },
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
              }
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
