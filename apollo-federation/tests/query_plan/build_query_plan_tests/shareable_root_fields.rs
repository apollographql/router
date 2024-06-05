#[test]
fn can_use_same_root_operation_from_multiple_subgraphs_in_parallel() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            me: User! @shareable
          }

          type User @key(fields: "id") {
            id: ID!
            prop1: String
          }
        "#,
        Subgraph2: r#"
          type Query {
            me: User! @shareable
          }

          type User @key(fields: "id") {
            id: ID!
            prop2: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            me {
              prop1
              prop2
            }
          }
        "#,
        @r###"
          QueryPlan {
            Parallel {
              Fetch(service: "Subgraph1") {
                {
                  me {
                    prop1
                  }
                }
              },
              Fetch(service: "Subgraph2") {
                {
                  me {
                    prop2
                  }
                }
              },
            },
          }
        "###
    );
}

#[test]
#[should_panic(
    expected = "Root nodes should have no remaining nodes unhandled, but got: [1 (missing: [2])]"
)]
fn handles_root_operation_shareable_in_many_subgraphs() {
    let planner = planner!(
        Subgraph1: r#"
        type User @key(fields: "id") {
          id: ID!
          f0: Int
          f1: Int
          f2: Int
          f3: Int
        }
        "#,
        Subgraph2: r#"
        type Query {
          me: User! @shareable
        }

        type User @key(fields: "id") {
          id: ID!
        }
        "#,
        Subgraph3: r#"
        type Query {
          me: User! @shareable
        }

        type User @key(fields: "id") {
          id: ID!
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          me {
            f0
            f1
            f2
            f3
          }
        }
        "#,
        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "Subgraph2") {
                {
                  me {
                    __typename
                    id
                  }
                }
              },
              Flatten(path: "me") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on User {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on User {
                      f0
                      f1
                      f2
                      f3
                    }
                  }
                },
              },
            },
          }
        "###
    );
}
