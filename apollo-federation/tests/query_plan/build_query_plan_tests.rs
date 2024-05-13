/// can use same root operation from multiple subgraphs in parallel
#[test]
fn shareable_root_fields() {
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
              }
              Fetch(service: "Subgraph2") {
                {
                  me {
                    prop2
                  }
                }
              }
            }
          }
        "###
    );
}

// TODO: port the rest of query-planner-js/src/__tests__/buildPlan.test.ts
