use apollo_compiler::ExecutableDocument;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::Supergraph;

mod include_skip;

#[test]
fn handles_simple_requires() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            a: Int @external
            b: Int @requires(fields: "a")
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              b
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
                  id
                  a
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    id
                    a
                  }
                } =>
                {
                  ... on T {
                    b
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
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn it_handles_multiple_requires_within_the_same_entity_fetch() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            is: [I!]!
          }
  
          interface I {
            id: ID!
            f: Int
            g: Int
          }
  
          type T1 implements I {
            id: ID!
            f: Int
            g: Int
          }
  
          type T2 implements I @key(fields: "id") {
            id: ID!
            f: Int!
            g: Int @external
          }
  
          type T3 implements I @key(fields: "id") {
            id: ID!
            f: Int
            g: Int @external
          }
        "#,
        Subgraph2: r#"
          type T2 @key(fields: "id") {
            id: ID!
            f: Int! @external
            g: Int @requires(fields: "f")
          }
  
          type T3 @key(fields: "id") {
            id: ID!
            f: Int @external
            g: Int @requires(fields: "f")
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            is {
              g
            }
          }
        "#,


      // The main goal of this test is to show that the 2 @requires for `f` gets handled seemlessly
      // into the same fetch group. But note that because the type for `f` differs, the 2nd instance
      // gets aliased (or the fetch would be invalid graphQL).
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                is {
                  __typename
                  ... on T1 {
                    g
                  }
                  ... on T2 {
                    __typename
                    id
                    f
                  }
                  ... on T3 {
                    __typename
                    id
                    f__alias_0: f
                  }
                }
              }
            },
            Flatten(path: "is.@") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T2 {
                    __typename
                    id
                    f
                  }
                  ... on T3 {
                    __typename
                    id
                    f
                  }
                } =>
                {
                  ... on T2 {
                    g
                  }
                  ... on T3 {
                    g
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
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn handles_multiple_requires_involving_different_nestedness() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            list: [Item]
          }
  
          type Item @key(fields: "user { id }") {
            id: ID!
            value: String
            user: User
          }
  
          type User @key(fields: "id") {
            id: ID!
            value: String
          }
        "#,
        Subgraph2: r#"
          type Item @key(fields: "user { id }") {
            user: User
            value: String @external
            computed: String @requires(fields: "user { value } value")
            computed2: String @requires(fields: "user { value }")
          }
  
          type User @key(fields: "id") {
            id: ID!
            value: String @external
            computed: String @requires(fields: "value")
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            list {
              computed
              computed2
              user {
                computed
              }
            }
          }
        "#,


      // The main goal of this test is to show that the 2 @requires for `f` gets handled seemlessly
      // into the same fetch group.
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                list {
                  __typename
                  user {
                    __typename
                    id
                    value
                  }
                  value
                }
              }
            },
            Parallel {
              Flatten(path: "list.@") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on Item {
                      __typename
                      user {
                        id
                        value
                      }
                      value
                    }
                  } =>
                  {
                    ... on Item {
                      computed
                      computed2
                    }
                  }
                },
              },
              Flatten(path: "list.@.user") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on User {
                      __typename
                      id
                      value
                    }
                  } =>
                  {
                    ... on User {
                      computed
                    }
                  }
                },
              },
            },
          },
        }
      "###
    );
}

/// require that depends on another require
#[test]
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")]
// TODO: investigate this failure
fn it_handles_simple_require_chain() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
            v: Int!
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v: Int! @external
            inner: Int! @requires(fields: "v")
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id") {
            id: ID!
            inner: Int! @external
            outer: Int! @requires(fields: "inner")
          }
        "#
    );
    // Ensures that if we only ask `outer`, we get everything needed in between.
    assert_plan!(
        &planner,
        r#"
          {
            t {
              outer
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
                  id
                  v
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    v
                    id
                  }
                } =>
                {
                  ... on T {
                    inner
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph3") {
                {
                  ... on T {
                    __typename
                    inner
                    id
                  }
                } =>
                {
                  ... on T {
                    outer
                  }
                }
              },
            },
          },
        }
      "###
    );

    // Ensures that manually asking for the required dependencies doesn't change anything
    // (note: technically it happens to switch the order of fields in the inputs of "Subgraph2"
    // so the plans are not 100% the same "string", which is why we inline it in both cases,
    // but that's still the same plan and a perfectly valid output).
    assert_plan!(
        &planner,
        r#"
          {
            t {
              v
              inner
              outer
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
                  id
                  v
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    id
                    v
                  }
                } =>
                {
                  ... on T {
                    inner
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph3") {
                {
                  ... on T {
                    __typename
                    inner
                    id
                  }
                } =>
                {
                  ... on T {
                    outer
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
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")]
// TODO: investigate this failure
fn it_handles_require_chain_not_ending_in_original_group() {
    // This is somewhat simiar to the 'simple require chain' case, but the chain does not
    // end in the group in which the query start
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v: Int! @external
            inner: Int! @requires(fields: "v")
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id") {
            id: ID!
            inner: Int! @external
            outer: Int! @requires(fields: "inner")
          }
        "#,
        Subgraph4: r#"
          type T @key(fields: "id") {
            id: ID!
            v: Int!
          }
        "#,
    );
    // Ensures that if we only ask `outer`, we get everything needed in between.
    assert_plan!(
        &planner,
        r#"
          {
            t {
              outer
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
                  id
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph4") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    v
                    id
                  }
                } =>
                {
                  ... on T {
                    inner
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph3") {
                {
                  ... on T {
                    __typename
                    inner
                    id
                  }
                } =>
                {
                  ... on T {
                    outer
                  }
                }
              },
            },
          },
        }
        "###
    );

    // Ensures that manually asking for the required dependencies doesn't change anything.
    assert_plan!(
        &planner,
        r#"
          {
            t {
              v
              inner
              outer
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
                  id
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph4") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    v
                    id
                  }
                } =>
                {
                  ... on T {
                    inner
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph3") {
                {
                  ... on T {
                    __typename
                    inner
                    id
                  }
                } =>
                {
                  ... on T {
                    outer
                  }
                }
              },
            },
          },
        }
        "###
    );
}

/// a chain of 10 requires
#[test]
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")]
// TODO: investigate this failure
fn it_handles_longer_require_chain() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
            v1: Int!
          }
        "#,
        Subgraph2: r#"
            type T @key(fields: "id") {
                id: ID!
                v1: Int! @external
                v2: Int! @requires(fields: "v1")
            }
        "#,
        Subgraph3: r#"
            type T @key(fields: "id") {
                id: ID!
                v2: Int! @external
                v3: Int! @requires(fields: "v2")
            }
        "#,
        Subgraph4: r#"
            type T @key(fields: "id") {
                id: ID!
                v3: Int! @external
                v4: Int! @requires(fields: "v3")
            }
        "#,
        Subgraph5: r#"
            type T @key(fields: "id") {
                id: ID!
                v4: Int! @external
                v5: Int! @requires(fields: "v4")
            }
        "#,
        Subgraph6: r#"
            type T @key(fields: "id") {
                id: ID!
                v5: Int! @external
                v6: Int! @requires(fields: "v5")
            }
        "#,
        Subgraph7: r#"
            type T @key(fields: "id") {
                id: ID!
                v6: Int! @external
                v7: Int! @requires(fields: "v6")
            }
        "#,
        Subgraph8: r#"
            type T @key(fields: "id") {
                id: ID!
                v7: Int! @external
                v8: Int! @requires(fields: "v7")
            }
        "#,
        Subgraph9: r#"
            type T @key(fields: "id") {
                id: ID!
                v8: Int! @external
                v9: Int! @requires(fields: "v8")
            }
        "#,
        Subgraph10: r#"
            type T @key(fields: "id") {
                id: ID!
                v9: Int! @external
                v10: Int! @requires(fields: "v9")
            }
        "#,
    );
    // Ensures that if we only ask `outer`, we get everything needed in between.
    assert_plan!(
        &planner,
        r#"
        {
          t {
            v10
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
                  id
                  v1
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    v1
                    id
                  }
                } =>
                {
                  ... on T {
                    v2
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph3") {
                {
                  ... on T {
                    __typename
                    v2
                    id
                  }
                } =>
                {
                  ... on T {
                    v3
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph4") {
                {
                  ... on T {
                    __typename
                    v3
                    id
                  }
                } =>
                {
                  ... on T {
                    v4
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph5") {
                {
                  ... on T {
                    __typename
                    v4
                    id
                  }
                } =>
                {
                  ... on T {
                    v5
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph6") {
                {
                  ... on T {
                    __typename
                    v5
                    id
                  }
                } =>
                {
                  ... on T {
                    v6
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph7") {
                {
                  ... on T {
                    __typename
                    v6
                    id
                  }
                } =>
                {
                  ... on T {
                    v7
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph8") {
                {
                  ... on T {
                    __typename
                    v7
                    id
                  }
                } =>
                {
                  ... on T {
                    v8
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph9") {
                {
                  ... on T {
                    __typename
                    v8
                    id
                  }
                } =>
                {
                  ... on T {
                    v9
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph10") {
                {
                  ... on T {
                    __typename
                    v9
                    id
                  }
                } =>
                {
                  ... on T {
                    v10
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
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")]
// TODO: investigate this failure
fn it_handles_complex_require_chain() {
    // Another "require chain" test but with more complexity as we have a require on multiple fields, some of which being
    // nested, and having requirements of their own.
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            inner1: Int!
            inner2_required: Int!
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id") {
            id: ID!
            inner2_required: Int! @external
            inner2: Int! @requires(fields: "inner2_required")
          }
        "#,
        Subgraph4: r#"
          type T @key(fields: "id") {
            id: ID!
            inner3: Inner3Type!
          }
  
          type Inner3Type @key(fields: "k3") {
            k3: ID!
          }
  
          type Inner4Type @key(fields: "k4") {
            k4: ID!
            inner4_required: Int!
          }
        "#,
        Subgraph5: r#"
          type T @key(fields: "id") {
            id: ID!
            inner1: Int! @external
            inner2: Int! @external
            inner3: Inner3Type! @external
            inner4: Inner4Type! @external
            inner5: Int! @external
            outer: Int!
              @requires(
                fields: "inner1 inner2 inner3 { inner3_nested } inner4 { inner4_nested } inner5"
              )
          }
  
          type Inner3Type @key(fields: "k3") {
            k3: ID!
            inner3_nested: Int!
          }
  
          type Inner4Type @key(fields: "k4") {
            k4: ID!
            inner4_nested: Int! @requires(fields: "inner4_required")
            inner4_required: Int! @external
          }
        "#,
        Subgraph6: r#"
          type T @key(fields: "id") {
            id: ID!
            inner4: Inner4Type!
          }
  
          type Inner4Type @key(fields: "k4") {
            k4: ID!
          }
        "#,
        Subgraph7: r#"
          type T @key(fields: "id") {
            id: ID!
            inner5: Int!
          }
        "#
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              outer
            }
          }
        "#,


      // This is a big plan, but afaict, this is optimal. That is, there is 3 main steps:
      // 1. it get the `id` for `T`, which is needed for anything else.
      // 2. it gets all the dependencies of for the @require on `outer` in parallel
      // 3. it finally get `outer`, passing all requirements as inputs.
      //
      // The 2nd step is the most involved, but it's just gathering the "outer" requirements in parallel,
      // while satisfying the "inner" requirements in each branch.
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                t {
                  __typename
                  id
                }
              }
            },
            Parallel {
              Sequence {
                Flatten(path: "t") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on T {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on T {
                        inner2_required
                        inner1
                      }
                    }
                  },
                },
                Flatten(path: "t") {
                  Fetch(service: "Subgraph3") {
                    {
                      ... on T {
                        __typename
                        inner2_required
                        id
                      }
                    } =>
                    {
                      ... on T {
                        inner2
                      }
                    }
                  },
                },
              },
              Flatten(path: "t") {
                Fetch(service: "Subgraph7") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      inner5
                    }
                  }
                },
              },
              Sequence {
                Flatten(path: "t") {
                  Fetch(service: "Subgraph6") {
                    {
                      ... on T {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on T {
                        inner4 {
                          __typename
                          k4
                        }
                      }
                    }
                  },
                },
                Flatten(path: "t.inner4") {
                  Fetch(service: "Subgraph4") {
                    {
                      ... on Inner4Type {
                        __typename
                        k4
                      }
                    } =>
                    {
                      ... on Inner4Type {
                        inner4_required
                      }
                    }
                  },
                },
                Flatten(path: "t.inner4") {
                  Fetch(service: "Subgraph5") {
                    {
                      ... on Inner4Type {
                        __typename
                        inner4_required
                        k4
                      }
                    } =>
                    {
                      ... on Inner4Type {
                        inner4_nested
                      }
                    }
                  },
                },
              },
              Sequence {
                Flatten(path: "t") {
                  Fetch(service: "Subgraph4") {
                    {
                      ... on T {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on T {
                        inner3 {
                          __typename
                          k3
                        }
                      }
                    }
                  },
                },
                Flatten(path: "t.inner3") {
                  Fetch(service: "Subgraph5") {
                    {
                      ... on Inner3Type {
                        __typename
                        k3
                      }
                    } =>
                    {
                      ... on Inner3Type {
                        inner3_nested
                      }
                    }
                  },
                },
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph5") {
                {
                  ... on T {
                    __typename
                    inner1
                    inner2
                    inner3 {
                      inner3_nested
                    }
                    inner4 {
                      inner4_nested
                    }
                    inner5
                    id
                  }
                } =>
                {
                  ... on T {
                    outer
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
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")]
// TODO: investigate this failure
fn it_handes_diamond_shape_depedencies() {
    // The idea of this test is that to be able to fulfill the @require in subgraph D, we need
    // both values from C for the @require and values from B for the key itself, but both
    // B and C can be queried directly after the initial query to A. This make the optimal query
    // plan diamond-shaped: after starting in A, we can get everything from B and C in
    // parallel, and then D needs to wait on both of those to run.

    let planner = planner!(
        A: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id1") {
            id1: ID!
          }
        "#,
        B: r#"
          type T @key(fields: "id1") @key(fields: "id2") {
            id1: ID!
            id2: ID!
            v1: Int
            v2: Int
          }
        "#,
        C: r#"
          type T @key(fields: "id1") {
            id1: ID!
            v3: Int
          }
        "#,
        D: r#"
          type T @key(fields: "id2") {
            id2: ID!
            v3: Int @external
            v4: Int @requires(fields: "v3")
          }
        "#
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              v1
              v2
              v3
              v4
            }
          }
        "#,


      // The optimal plan should:
      // 1. fetch id1 from A
      // 2. from that, it can both (in parallel):
      //   - get id2, v1 and v2 from B
      //   - get v3 from C
      // 3. lastly, once both of those return, it can get v4 from D as it has all requirement
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "A") {
              {
                t {
                  __typename
                  id1
                }
              }
            },
            Parallel {
              Flatten(path: "t") {
                Fetch(service: "B") {
                  {
                    ... on T {
                      __typename
                      id1
                    }
                  } =>
                  {
                    ... on T {
                      __typename
                      id2
                      v1
                      v2
                      id1
                    }
                  }
                },
              },
              Flatten(path: "t") {
                Fetch(service: "C") {
                  {
                    ... on T {
                      __typename
                      id1
                    }
                  } =>
                  {
                    ... on T {
                      v3
                    }
                  }
                },
              },
            },
            Flatten(path: "t") {
              Fetch(service: "D") {
                {
                  ... on T {
                    __typename
                    v3
                    id2
                  }
                } =>
                {
                  ... on T {
                    v4
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
fn it_can_require_at_inaccessible_fields() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            one: One
            onlyIn1: Int
          }
  
          type One @key(fields: "id") {
            id: ID!
            a: String @inaccessible
            onlyIn1: Int
          }
        "#,
        Subgraph2: r#"
          type Query {
            onlyIn2: Int
          }
  
          type One @key(fields: "id") {
            id: ID!
            a: String @external
            b: String @requires(fields: "a")
            onlyIn2: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            one {
              b
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                one {
                  __typename
                  id
                  a
                }
              }
            },
            Flatten(path: "one") {
              Fetch(service: "Subgraph2") {
                {
                  ... on One {
                    __typename
                    id
                    a
                  }
                } =>
                {
                  ... on One {
                    b
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
#[should_panic(expected = "An internal error has occurred, please report this bug to Apollo")]
// TODO: investigate this failure
fn it_require_of_multiple_field_when_one_is_also_a_key_to_reach_another() {
    // The specificity of this example is that we `T.v` requires 2 fields `req1`
    // and `req2`, but `req1` is also a key to get `req2`. This dependency was
    // confusing a previous version of the code (which, when gathering the
    // "createdGroups" for `T.v` @requires, was using the group for `req1` twice
    // separatly (instead of recognizing it was the same group), and this was
    // confusing the rest of the code was wasn't expecting it.
    let planner = planner!(
        A: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id1") @key(fields: "req1") {
            id1: ID!
            req1: Int
          }
        "#,
        B: r#"
          type T @key(fields: "id1") {
            id1: ID!
            req1: Int @external
            req2: Int @external
            v: Int @requires(fields: "req1 req2")
          }
        "#,
        C: r#"
          type T @key(fields: "req1") {
            req1: Int
            req2: Int
          }
        "#
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              v
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "A") {
              {
                t {
                  __typename
                  id1
                  req1
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "C") {
                {
                  ... on T {
                    __typename
                    req1
                  }
                } =>
                {
                  ... on T {
                    req2
                  }
                }
              },
            },
            Flatten(path: "t") {
              Fetch(service: "B") {
                {
                  ... on T {
                    __typename
                    req1
                    req2
                    id1
                  }
                } =>
                {
                  ... on T {
                    v
                  }
                }
              },
            },
          },
        }
      "###
    );
}
