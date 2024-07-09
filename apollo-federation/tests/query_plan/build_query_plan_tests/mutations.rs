const SUBGRAPH_A: &str = r#"
      type Foo @key(fields: "id") {
        id: ID!
        bar: String
      }

      type Query {
        foo: Foo
      }

      type Mutation {
        updateFooInA: Foo
      }
"#;

const SUBGRAPH_B: &str = r#"
      type Mutation {
        updateFooInB: Foo
      }

      type Foo @key(fields: "id") {
        id: ID!
        baz: Int
      }
"#;

#[test]
fn adjacent_mutations_get_merged() {
    let planner = planner!(
        SubgraphA: SUBGRAPH_A,
        SubgraphB: SUBGRAPH_B,
    );
    assert_plan!(
        &planner,
        r#"
        mutation TestMutation {
          updateInAOne: updateFooInA {
            id
            bar
          }
          updateInBOne: updateFooInB {
            id
            baz
          }
          updateInATwo: updateFooInA {
            id
            bar
          }
        }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "SubgraphA") {
              {
                updateInAOne: updateFooInA {
                  id
                  bar
                }
              }
            },
            Fetch(service: "SubgraphB") {
              {
                updateInBOne: updateFooInB {
                  id
                  baz
                }
              }
            },
            Fetch(service: "SubgraphA") {
              {
                updateInATwo: updateFooInA {
                  id
                  bar
                }
              }
            },
          },
        }
        "###
    );
}
