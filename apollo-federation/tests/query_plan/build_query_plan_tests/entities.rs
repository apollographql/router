// TODO this test shows inefficient QP where we make multiple parallel
// fetches of the same entity from the same subgraph but for different paths
#[test]
fn inefficient_entity_fetches_to_same_subgraph() {
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
                  id
                  w
                  inner {
                    v {
                      x
                    }
                    w
                  }
                }
                outer2 {
                  __typename
                  id
                  w
                  inner {
                    v {
                      x
                    }
                    w
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
}
