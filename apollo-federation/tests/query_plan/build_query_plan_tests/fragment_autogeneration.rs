use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

const SUBGRAPH: &str = r#"
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
            ...a
            ... on B {
              z
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
            ...b
          }
        }

        fragment a on A {
          x
          y
        }

        fragment b on A {
          x
          y
          t {
            __typename
            ...a
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
            ...a
          }
        }

        fragment a on A {
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
            ...a
          }
          t2 {
            __typename
            ...a
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
            ...a
          }
          t2 {
            __typename
            ...b
          }
        }

        fragment a on A {
          x
          y
        }

        fragment b on A {
          y
          z
        }
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
