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
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")] // TODO: fix bug FED-230
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

#[test]
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn it_works_on_unions() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            noProvides: U
            withProvidesForT1: U @provides(fields: "... on T1 { a }")
            withProvidesForBoth: U
              @provides(fields: "... on T1 { a } ... on T2 {b}")
          }
  
          union U = T1 | T2
  
          type T1 @key(fields: "id") {
            id: ID!
            a: Int @external
          }
  
          type T2 @key(fields: "id") {
            id: ID!
            a: Int
            b: Int @external
          }
        "#,
        Subgraph2: r#"
          type T1 @key(fields: "id") {
            id: ID!
            a: Int @shareable
          }
  
          type T2 @key(fields: "id") {
            id: ID!
            b: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            noProvides {
              ... on T1 {
                a
              }
              ... on T2 {
                a
                b
              }
            }
          }
        "#,
        // This is our sanity check: we first query _without_ the provides
        // to make sure we _do_ need to go the the second subgraph.
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            noProvides {
              ... on T1 {
                __typename
                id
              }
              ... on T2 {
                __typename
                id
                a
              }
            }
          }
        },
        Parallel {
          Flatten(path: "noProvides") {
            Fetch(service: "Subgraph2") {
              {
                ... on T2 {
                  __typename
                  id
                }
              } =>
              {
                ... on T2 {
                  b
                }
              }
            },
          },
          Flatten(path: "noProvides") {
            Fetch(service: "Subgraph2") {
              {
                ... on T1 {
                  __typename
                  id
                }
              } =>
              {
                ... on T1 {
                  a
                }
              }
            },
          },
        },
      },
    }
    "###
    );

    // Ensuring that querying only `a` can be done with subgraph1 only when provided.
    assert_plan!(
        &planner,
        r#"
          {
            withProvidesForT1 {
              ... on T1 {
                a
              }
              ... on T2 {
                a
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          withProvidesForT1 {
            ... on T1 {
              a
            }
            ... on T2 {
              a
            }
          }
        }
      },
    }
    "###
    );

    // But ensure that querying `b` still goes to subgraph2 if only a is provided.
    assert_plan!(
        &planner,
        r#"
          {
            withProvidesForT1 {
              ... on T1 {
                a
              }
              ... on T2 {
                a
                b
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            withProvidesForT1 {
              ... on T1 {
                a
              }
              ... on T2 {
                __typename
                id
                a
              }
            }
          }
        },
        Flatten(path: "withProvidesForT1") {
          Fetch(service: "Subgraph2") {
            {
              ... on T2 {
                __typename
                id
              }
            } =>
            {
              ... on T2 {
                b
              }
            }
          },
        },
      },
    }
    "###
    );

    // Lastly, if both are provided, ensures we only hit subgraph1.
    assert_plan!(
        &planner,
        r#"
          {
            withProvidesForBoth {
              ... on T1 {
                a
              }
              ... on T2 {
                a
                b
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              withProvidesForBoth {
                __typename
                ... on T1 {
                  a
                }
                ... on T2 {
                  a
                  b
                }
              }
            }
          },
        }
        "###
    );
}

#[test]
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn it_allow_providing_fields_for_only_some_subtype() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            noProvides: I
            withProvidesOnA: I @provides(fields: "... on T2 { a }")
            withProvidesOnB: I @provides(fields: "... on T2 { b }")
          }
  
          interface I {
            a: Int
            b: Int
          }
  
          type T1 implements I @key(fields: "id") {
            id: ID!
            a: Int
            b: Int @external
          }
  
          type T2 implements I @key(fields: "id") {
            id: ID!
            a: Int @external
            b: Int @external
          }
        "#,
        Subgraph2: r#"
          type T1 @key(fields: "id") {
            id: ID!
            b: Int
          }
  
          type T2 @key(fields: "id") {
            id: ID!
            a: Int @shareable
            b: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            noProvides {
              a
              b
            }
          }
        "#,


      // This is our sanity check: we first query _without_ the provides
      // to make sure we _do_ need to go the the second subgraph.
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
                    a
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
                    b
                  }
                  ... on T2 {
                    a
                    b
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
            withProvidesOnA {
              a
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              withProvidesOnA {
                __typename
                ... on T1 {
                  a
                }
                ... on T2 {
                  a
                }
              }
            }
          },
        }
        "###
    );

    // Ensuring that for `b`, only the T2 value is provided by subgraph1.
    assert_plan!(
        &planner,
        r#"
          {
            withProvidesOnB {
              b
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                withProvidesOnB {
                  __typename
                  ... on T1 {
                    __typename
                    id
                  }
                  ... on T2 {
                    b
                  }
                }
              }
            },
            Flatten(path: "withProvidesOnB") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T1 {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T1 {
                    b
                  }
                }
              },
            },
          },
        }
        "###
    );

    // But if we only query for T2, then no reason to go to subgraph2.
    assert_plan!(
        &planner,
        r#"
          {
            withProvidesOnB {
              ... on T2 {
                b
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              withProvidesOnB {
                __typename
                ... on T2 {
                  b
                }
              }
            }
          },
        }
        "###
    );
}

#[test]
#[should_panic(expected = "Subgraph unexpectedly does not use federation spec")]
// TODO: investigate this failure
fn it_works_with_type_condition_even_for_types_only_reachable_by_the_at_provides() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            noProvides: E
            withProvides: E @provides(fields: "i { a ... on T1 { b } }")
          }
  
          type E @key(fields: "id") {
            id: ID!
            i: I @external
          }
  
          interface I {
            a: Int
          }
  
          type T1 implements I @key(fields: "id") {
            id: ID!
            a: Int @external
            b: Int @external
          }
  
          type T2 implements I @key(fields: "id") {
            id: ID!
            a: Int @external
          }
        "#,
        Subgraph2: r#"
          type E @key(fields: "id") {
            id: ID!
            i: I @shareable
          }
  
          interface I {
            a: Int
          }
  
          type T1 implements I @key(fields: "id") {
            id: ID!
            a: Int @shareable
            b: Int @shareable
          }
  
          type T2 implements I @key(fields: "id") {
            id: ID!
            a: Int @shareable
            c: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            noProvides {
              i {
                a
                ... on T1 {
                  b
                }
                ... on T2 {
                  c
                }
              }
            }
          }
        "#,


      // This is our sanity check: we first query _without_ the provides to make sure we _do_ need to
      // go the the second subgraph for everything.
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                noProvides {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "noProvides") {
              Fetch(service: "Subgraph2") {
                {
                  ... on E {
                    __typename
                    id
                  }
                } =>
                {
                  ... on E {
                    i {
                      __typename
                      a
                      ... on T1 {
                        b
                      }
                      ... on T2 {
                        c
                      }
                    }
                  }
                }
              },
            },
          },
        }
      "###
    );

    // But the same operation with the provides allow to get what is provided from the first subgraph.
    assert_plan!(
        &planner,
        r#"
          {
            withProvides {
              i {
                a
                ... on T1 {
                  b
                }
                ... on T2 {
                  c
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
                withProvides {
                  i {
                    __typename
                    a
                    ... on T1 {
                      b
                    }
                    ... on T2 {
                      __typename
                      id
                    }
                  }
                }
              }
            },
            Flatten(path: "withProvides.i") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T2 {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T2 {
                    c
                  }
                }
              },
            },
          },
        }
      "###
    );
}
