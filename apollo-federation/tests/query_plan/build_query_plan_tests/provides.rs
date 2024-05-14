#[test]
fn it_works_with_nested_provides() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          doSomething: Response
          doSomethingWithProvides: Response
            @provides(
              fields: "responseValue { subResponseValue { subSubResponseValue } }"
            )
        }

        type Response {
          responseValue: SubResponse
        }

        type SubResponse {
          subResponseValue: SubSubResponse
        }

        type SubSubResponse @key(fields: "id") {
          id: ID!
          subSubResponseValue: Int @external
        }
        "#,
        Subgraph2: r#"
        type SubSubResponse @key(fields: "id") {
          id: ID!
          subSubResponseValue: Int @shareable
        }
        "#,
    );
    // This is our sanity check: we first query _without_ the provides
    // to make sure we _do_ need to go the the second subgraph.
    assert_plan!(
        &planner,
        r#"
        {
          doSomething {
            responseValue {
              subResponseValue {
                subSubResponseValue
              }
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                doSomething {
                  responseValue {
                    subResponseValue {
                      __typename
                      id
                    }
                  }
                }
              }
            },
            Flatten(path: "doSomething.responseValue.subResponseValue") {
              Fetch(service: "Subgraph2") {
                {
                  ... on SubSubResponse {
                    __typename
                    id
                  }
                } =>
                {
                  ... on SubSubResponse {
                    subSubResponseValue
                  }
                }
              },
            },
          },
        }
        "###
    );
    // And now make sure with the provides we do only get a fetch to subgraph1
    assert_plan!(
        &planner,
        r#"
        {
          doSomethingWithProvides {
            responseValue {
              subResponseValue {
                subSubResponseValue
              }
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              doSomethingWithProvides {
                responseValue {
                  subResponseValue {
                    subSubResponseValue
                  }
                }
              }
            }
          },
        }
        "###
    );
}

#[test]
#[should_panic] // TODO: fix bug FED-230
fn it_works_on_interfaces() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          noProvides: I
          withProvides: I @provides(fields: "v { a }")
        }

        interface I {
          v: Value
        }

        type Value {
          a: Int @shareable
        }

        type T1 implements I @key(fields: "id") {
          id: ID!
          v: Value @external
        }

        type T2 implements I @key(fields: "id") {
          id: ID!
          v: Value @external
        }
        "#,
        Subgraph2: r#"
        type Value {
          a: Int @shareable
          b: Int
        }

        type T1 @key(fields: "id") {
          id: ID!
          v: Value @shareable
        }

        type T2 @key(fields: "id") {
          id: ID!
          v: Value @shareable
        }
        "#,
    );
    // This is our sanity check: we first query _without_ the provides
    // to make sure we _do_ need to go the the second subgraph.
    assert_plan!(
        &planner,
        r#"
        {
          noProvides {
            v {
              a
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                noProvides {
                  __typename
                  ... on T1 {
                    __typename
                    id
                  }
                  ... on T2 {
                    __typename
                    id
                  }
                }
              }
            },
            Flatten(path: "noProvides") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T1 {
                    __typename
                    id
                  }
                  ... on T2 {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T1 {
                    v {
                      a
                    }
                  }
                  ... on T2 {
                    v {
                      a
                    }
                  }
                }
              },
            },
          },
        }
        "###
    );
    // Ensuring that querying only `a` can be done with subgraph1 only.
    assert_plan!(
        &planner,
        r#"
        {
          withProvides {
            v {
              a
            }
          }
        }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              withProvides {
                __typename
                v {
                  a
                }
              }
            }
          },
        }
        "###
    );
}
