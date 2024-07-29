use crate::query_plan::build_query_plan_support::find_fetch_nodes_for_subgraph;

const SUBGRAPH_A: &str = r#"
      directive @operation on MUTATION | QUERY | SUBSCRIPTION
      directive @field on FIELD

      type Foo @key(fields: "id") {
        id: ID!
        bar: String
        t: T!
      }

      type T @key(fields: "id") {
        id: ID!
      }

      type Query {
        foo: Foo
      }

      type Mutation {
        updateFoo(bar: String): Foo
      }
"#;

const SUBGRAPH_B: &str = r#"
      directive @operation on MUTATION | QUERY | SUBSCRIPTION
      directive @field on FIELD

      type Foo @key(fields: "id") {
        id: ID!
        baz: Int
      }

      type T @key(fields: "id") {
        id: ID!
        f1: String
      }
"#;

#[test]
fn test_if_directives_at_the_operation_level_are_passed_down_to_subgraph_queries() {
    let planner = planner!(
        subgraphA: SUBGRAPH_A,
        subgraphB: SUBGRAPH_B,
    );
    let plan = assert_plan!(
      &planner,
      r#"
        query Operation @operation {
          foo @field {
            bar @field
            baz @field
            t @field {
              f1 @field
            }
          }
        }
      "#,
      @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "subgraphA") {
            {
              foo @field {
                __typename
                id
                bar @field
                t @field {
                  __typename
                  id
                }
              }
            }
          },
          Parallel {
            Flatten(path: "foo.t") {
              Fetch(service: "subgraphB") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    f1 @field
                  }
                }
              },
            },
            Flatten(path: "foo") {
              Fetch(service: "subgraphB") {
                {
                  ... on Foo {
                    __typename
                    id
                  }
                } =>
                {
                  ... on Foo {
                    baz @field
                  }
                }
              },
            },
          },
        },
      }
      "###
    );
    let a_fetch_nodes = find_fetch_nodes_for_subgraph("subgraphA", &plan);
    assert_eq!(a_fetch_nodes.len(), 1);
    // Note: The query is expected to carry the `@operation` directive.
    insta::assert_snapshot!(a_fetch_nodes[0].operation_document, @r#"
      query Operation__subgraphA__0 @operation {
        foo @field {
          __typename
          id
          bar @field
          t @field {
            __typename
            id
          }
        }
      }
    "#);

    let b_fetch_nodes = find_fetch_nodes_for_subgraph("subgraphB", &plan);
    assert_eq!(b_fetch_nodes.len(), 2);
    // Note: The query is expected to carry the `@operation` directive.
    insta::assert_snapshot!(b_fetch_nodes[0].operation_document, @r#"
      query Operation__subgraphB__1($representations: [_Any!]!) @operation {
        _entities(representations: $representations) {
          ... on T {
            f1 @field
          }
        }
      }
    "#);
    // Note: The query is expected to carry the `@operation` directive.
    insta::assert_snapshot!(b_fetch_nodes[1].operation_document, @r#"
      query Operation__subgraphB__2($representations: [_Any!]!) @operation {
        _entities(representations: $representations) {
          ... on Foo {
            baz @field
          }
        }
      }
    "#);
}

#[test]
fn test_if_directives_on_mutations_are_passed_down_to_subgraph_queries() {
    let planner = planner!(
        subgraphA: SUBGRAPH_A,
        subgraphB: SUBGRAPH_B,
    );
    let plan = assert_plan!(
      &planner,
      r#"
        mutation TestMutation @operation {
          updateFoo(bar: "something") @field {
            id @field
            bar @field
          }
        }
      "#,
      @r###"
      QueryPlan {
        Fetch(service: "subgraphA") {
          {
            updateFoo(bar: "something") @field {
              id @field
              bar @field
            }
          }
        },
      }
      "###
    );

    let fetch_nodes = find_fetch_nodes_for_subgraph("subgraphA", &plan);
    assert_eq!(fetch_nodes.len(), 1);
    // Note: The query is expected to carry the `@operation` directive.
    insta::assert_snapshot!(fetch_nodes[0].operation_document, @r#"
      mutation TestMutation__subgraphA__0 @operation {
        updateFoo(bar: "something") @field {
          id @field
          bar @field
        }
      }
    "#);
}

#[test]
fn test_if_directives_with_arguments_applied_on_queries_are_ok() {
    let planner = planner!(
      Subgraph1: r#"
        directive @noArgs on QUERY
        directive @withArgs(arg1: String) on QUERY

        type Query {
          test: String!
        }
      "#,
      Subgraph2: r#"
        directive @noArgs on QUERY
        directive @withArgs(arg1: String) on QUERY
      "#,
    );
    let plan = assert_plan!(
      &planner,
      r#"
        query @noArgs @withArgs(arg1: "hi") {
          test
        }
        "#,
      @r###"
      QueryPlan {
        Fetch(service: "Subgraph1") {
          {
            test
          }
        },
      }
      "###
    );

    let fetch_nodes = find_fetch_nodes_for_subgraph("Subgraph1", &plan);
    assert_eq!(fetch_nodes.len(), 1);
    // Note: The query is expected to carry the `@noArgs` and `@withArgs` directive.
    insta::assert_snapshot!(fetch_nodes[0].operation_document, @r#"
      query @noArgs @withArgs(arg1: "hi") {
        test
      }
    "#);
}

#[test]
fn subgraph_query_retains_the_query_variables_used_in_the_directives_applied_to_the_query() {
    let planner = planner!(
      Subgraph1: r#"
        directive @withArgs(arg1: String) on QUERY

        type Query {
          test: String!
        }
      "#,
      Subgraph2: r#"
        directive @withArgs(arg1: String) on QUERY
      "#,
    );

    let plan = assert_plan!(
      &planner,
      r#"
        query testQuery($some_var: String!) @withArgs(arg1: $some_var) {
            test
          }
        "#,
      @r#""#
    );

    let fetch_nodes = find_fetch_nodes_for_subgraph("Subgraph1", &plan);
    assert_eq!(fetch_nodes.len(), 1);
    // Note: `($some_var: String!)` used to be missing.
    insta::assert_snapshot!(fetch_nodes[0].operation_document, @r#"
      query testQuery__Subgraph1__0($some_var: String!) @withArgs(arg1: $some_var) {
        test
      }
    "#);
}
