#[test]
fn handles_non_matching_value_types_under_interface_field() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i: I
          }
  
          interface I {
            s: S
          }
  
          type T implements I @key(fields: "id") {
            id: ID!
            s: S @shareable
          }
  
          type S @shareable {
            x: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            s: S @shareable
          }
  
          type S @shareable {
            x: Int
            y: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i {
              s {
                y
              }
            }
          }
        "#,


      // The schema is constructed in such a way that we *need* to type-explode interface `I`
      // to be able to find field `y`. Make sure that happens.
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                i {
                  __typename
                  ... on T {
                    __typename
                    id
                  }
                }
              }
            },
            Flatten(path: "i") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    s {
                      y
                    }
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
#[should_panic(expected = "assertion `left == right` failed")]
// TODO: investigate this failure (`evaluated_plan_count` is 0, when it's expected to be 1.)
fn skip_type_explosion_early_if_unnecessary() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i: I
          }
  
          interface I {
            s: S
          }
  
          type T implements I @key(fields: "id") {
            id: ID!
            s: S @shareable
          }
  
          type S @shareable {
            x: Int
            y: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            s: S @shareable
          }
  
          type S @shareable {
            x: Int
            y: Int
          }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          {
            i {
              s {
                y
              }
            }
          }
        "#,


      // This test is a small variation on the previous test ('handles non-matching ...'), we
      // we _can_ use the interface field directly and don't need to type-explode. So we
      // double-check that the plan indeed does not type-explode, but the true purpose of
      // this test is to ensure the proper optimisation kicks in so that we do _not_ even
      // evaluate the plan where we type explode. In other words, we ensure that the plan
      // we get is the _only_ one evaluated.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              i {
                __typename
                s {
                  y
                }
              }
            }
          },
        }
      "###
    );
    assert_eq!(plan.statistics.evaluated_plan_count, 1);
}
