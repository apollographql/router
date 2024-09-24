use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

fn config_with_defer() -> QueryPlannerConfig {
    let mut config = QueryPlannerConfig::default();
    config.incremental_delivery.enable_defer = true;
    config
}

#[test]
fn defer_test_handles_simple_defer_without_defer_enabled() {
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
            v1: Int
            v2: Int
        }
        "#,
    );
    // without defer-support enabled
    assert_plan!(planner,
        r#"
            {
              t {
                v1
                ... @defer {
                  v2
                }
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
          Fetch(service: "Subgraph2") {
            {
              ... on T {
                __typename
                id
              }
            } =>
            {
              ... on T {
                v1
                v2
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
fn defer_test_normalizes_if_false() {
    let planner = planner!(
        config = config_with_defer(),
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
            v1: Int
            v2: Int
        }
        "#,
    );
    assert_plan!(planner,
        r#"
            {
              t {
                v1
                ... @defer(if: false) {
                  v2
                }
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
          Fetch(service: "Subgraph2") {
            {
              ... on T {
                __typename
                id
              }
            } =>
            {
              ... on T {
                v1
                v2
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
fn defer_test_normalizes_if_true() {
    let planner = planner!(
        config = config_with_defer(),
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
            v1: Int
            v2: Int
        }
        "#,
    );
    assert_plan!(planner,
        r#"
            {
              t {
                v1
                ... @defer(if: true) {
                  v2
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v1
            }
          }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                t {
                  __typename
                  id
                }
              }
            },
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
                    v1
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              v2
            }:
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
                    v2
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_handles_simple_defer_with_defer_enabled() {
    let planner = planner!(
        config = config_with_defer(),
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
            v1: Int
            v2: Int
        }
        "#,
    );
    assert_plan!(planner,
        r#"
            {
              t {
                v1
                ... @defer {
                  v2
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v1
            }
          }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                t {
                  __typename
                  id
                }
              }
            },
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
                    v1
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              v2
            }:
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
                    v2
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_non_router_based_defer_case_one() {
    // @defer on value type
    let planner = planner!(
        config = config_with_defer(),
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
            v: V
        }

        type V {
            a: Int
            b: Int
        }
        "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                v {
                  a
                  ... @defer {
                    b
                  }
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v {
                a
              }
            }
          }:
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
              Fetch(service: "Subgraph2") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v {
                      a
                      b
                    }
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [], path: "t/v") {
            {
              b
            }:
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_non_router_based_defer_case_two() {
    // @defer on entity but with no @key
    // While the @defer in the operation is on an entity, the @key in the first subgraph
    // is explicitely marked as non-resovable, so we cannot use it to actually defer the
    // fetch to `v1`. Note that example still compose because, defer excluded, `v1` can
    // still be fetched for all queries (which is only `t` here).
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
        type Query {
            t: T
        }

        type T @key(fields: "id", resolvable: false) {
            id: ID!
            v1: String
        }
        "#,
        Subgraph2: r#"
        type T @key(fields: "id") {
            id: ID!
            v2: String
        }
        "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                ... @defer {
                  v1
                }
                v2
              }
            }
        "#,
        @r###"
          QueryPlan {
            Defer {
              Primary {
                {
                  t {
                    v2
                  }
                }:
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
                },
              }, [
                Deferred(depends: [], path: "t") {
                  {
                    v1
                  }:
                },
              ]
            },
          }
        "###
    );
}

#[test]
fn defer_test_non_router_based_defer_case_three() {
    // @defer on value type but with entity afterwards
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
        }

        type U @key(fields: "id") {
            id: ID!
            x: Int
        }
        "#,

        Subgraph2: r#"
        type T @key(fields: "id") {
            id: ID!
            v: V
        }

        type V {
            a: Int
            u: U
        }

        type U @key(fields: "id") {
            id: ID!
        }
        "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                v {
                  a
                  ... @defer {
                    u {
                      x
                    }
                  }
                }
              }
            }
        "#,
        @r###"
          QueryPlan {
            Defer {
              Primary {
                {
                  t {
                    v {
                      a
                    }
                  }
                }:
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
                    Fetch(service: "Subgraph2", id: 0) {
                      {
                        ... on T {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on T {
                          v {
                            a
                            u {
                              __typename
                              id
                            }
                          }
                        }
                      }
                    },
                  },
                },
              }, [
                Deferred(depends: [0], path: "t/v") {
                  {
                    u {
                      x
                    }
                  }:
                  Flatten(path: "t.v.u") {
                    Fetch(service: "Subgraph1") {
                      {
                        ... on U {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on U {
                          x
                        }
                      }
                    },
                  },
                },
              ]
            },
          }
        "###
    );
}

#[test]
fn defer_test_defer_resuming_in_the_same_subgraph() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
            v1: String
          }
          "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              v0
              ... @defer {
                v1
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v0
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                v0
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              v1
            }:
            Flatten(path: "t") {
              Fetch(service: "Subgraph1") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v1
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_multiple_fields_in_different_subgraphs() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
            v1: String
          }
        "#,

        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v2: String
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id") {
            id: ID!
            v3: String
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              v0
              ... @defer {
                v1
                v2
                v3
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Defer {
            Primary {
              {
                t {
                  v0
                }
              }:
              Fetch(service: "Subgraph1", id: 0) {
                {
                  t {
                    __typename
                    v0
                    id
                  }
                }
              },
            }, [
              Deferred(depends: [0], path: "t") {
                {
                  v1
                  v2
                  v3
                }:
                Parallel {
                  Flatten(path: "t") {
                    Fetch(service: "Subgraph1") {
                      {
                        ... on T {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on T {
                          v1
                        }
                      }
                    },
                  },
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
                },
              },
            ]
          },
        }
        "###
    );
}

#[test]
fn defer_test_multiple_non_nested_defer_plus_label_handling() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
            v1: String
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v2: String
            v3: U
          }

          type U @key(fields: "id") {
            id: ID!
          }
        "#,
        Subgraph3: r#"
          type U @key(fields: "id") {
            id: ID!
            x: Int
            y: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              v0
              ... @defer(label: "defer_v1") {
                v1
              }
              ... @defer {
                v2
              }
              v3 {
                x
                ... @defer(label: "defer_in_v3") {
                  y
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v0
              v3 {
                x
              }
            }
          }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                t {
                  __typename
                  id
                  v0
                }
              }
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph2", id: 1) {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v3 {
                      __typename
                      id
                    }
                  }
                }
              },
            },
            Flatten(path: "t.v3") {
              Fetch(service: "Subgraph3") {
                {
                  ... on U {
                    __typename
                    id
                  }
                } =>
                {
                  ... on U {
                    x
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              v2
            }:
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
                    v2
                  }
                }
              },
            },
          },
          Deferred(depends: [0], path: "t", label: "defer_v1") {
            {
              v1
            }:
            Flatten(path: "t") {
              Fetch(service: "Subgraph1") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v1
                  }
                }
              },
            },
          },
          Deferred(depends: [1], path: "t/v3", label: "defer_in_v3") {
            {
              y
            }:
            Flatten(path: "t.v3") {
              Fetch(service: "Subgraph3") {
                {
                  ... on U {
                    __typename
                    id
                  }
                } =>
                {
                  ... on U {
                    y
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_nested_defer_on_entities() {
    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              me: User
            }

            type User @key(fields: "id") {
              id: ID!
              name: String
            }
          "#,
          Subgraph2: r#"
            type User @key(fields: "id") {
              id: ID!
              messages: [Message]
            }

            type Message @key(fields: "id") {
              id: ID!
              body: String
              author: User
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              me {
                name
                ... on User @defer {
                  messages {
                    body
                    author {
                      name
                      ... @defer {
                        messages {
                          body
                        }
                      }
                    }
                  }
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            me {
              name
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              me {
                __typename
                name
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "me") {
            Defer {
              Primary {
                {
                  ... on User {
                    messages {
                      body
                      author {
                        name
                      }
                    }
                  }
                }:
                Sequence {
                  Flatten(path: "me") {
                    Fetch(service: "Subgraph2", id: 1) {
                      {
                        ... on User {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on User {
                          messages {
                            body
                            author {
                              __typename
                              id
                            }
                          }
                        }
                      }
                    },
                  },
                  Flatten(path: "me.messages.@.author") {
                    Fetch(service: "Subgraph1") {
                      {
                        ... on User {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on User {
                          name
                        }
                      }
                    },
                  },
                },
              }, [
                Deferred(depends: [1], path: "me/messages/author") {
                  {
                    messages {
                      body
                    }
                  }:
                  Flatten(path: "me.messages.@.author") {
                    Fetch(service: "Subgraph2") {
                      {
                        ... on User {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on User {
                          messages {
                            body
                          }
                        }
                      }
                    },
                  },
                },
              ]
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_on_value_types() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            me: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        Subgraph2: r#"
          type User @key(fields: "id") {
            id: ID!
            messages: [Message]
          }

          type Message {
            id: ID!
            body: MessageBody
          }

          type MessageBody {
            paragraphs: [String]
            lines: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            me {
              ... @defer {
                messages {
                  ... @defer {
                    body {
                      lines
                    }
                  }
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          Fetch(service: "Subgraph1", id: 0) {
            {
              me {
                __typename
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "me") {
            Defer {
              Primary {
                Flatten(path: "me") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on User {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on User {
                        messages {
                          body {
                            lines
                          }
                        }
                      }
                    }
                  },
                },
              }, [
                Deferred(depends: [], path: "me/messages") {
                  {
                    body {
                      lines
                    }
                  }:
                },
              ]
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_direct_nesting_on_entity() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            me: User
          }

          type User @key(fields: "id") {
            id: ID!
            name: String
          }
        "#,
        Subgraph2: r#"
          type User @key(fields: "id") {
            id: ID!
            age: Int
            address: String
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            me {
              name
              ... @defer {
                age
                ... @defer {
                  address
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            me {
              name
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              me {
                __typename
                name
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "me") {
            Defer {
              Primary {
                {
                  age
                }:
                Flatten(path: "me") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on User {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on User {
                        age
                      }
                    }
                  },
                },
              }, [
                Deferred(depends: [0], path: "me") {
                  {
                    address
                  }:
                  Flatten(path: "me") {
                    Fetch(service: "Subgraph2") {
                      {
                        ... on User {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on User {
                          address
                        }
                      }
                    },
                  },
                },
              ]
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_direct_nesting_on_value_type() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            me: User
          }

          type User {
            id: ID!
            name: String
            age: Int
            address: String
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            me {
              name
              ... @defer {
                age
                ... @defer {
                  address
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            me {
              name
            }
          }:
          Fetch(service: "Subgraph1") {
            {
              me {
                name
                age
                address
              }
            }
          },
        }, [
          Deferred(depends: [], path: "me") {
            Defer {
              Primary {
                {
                  age
                }
              }, [
                Deferred(depends: [], path: "me") {
                  {
                    address
                  }:
                },
              ]
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_on_enity_but_with_unuseful_key() {
    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type T {
              id: ID! @shareable
              a: Int
              b: Int
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                ... @defer {
                  a
                  ... @defer {
                    b
                  }
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          Fetch(service: "Subgraph1") {
            {
              t {
                a
                b
              }
            }
          },
        }, [
          Deferred(depends: [], path: "t") {
            Defer {
              Primary {
                {
                  a
                }
              }, [
                Deferred(depends: [], path: "t") {
                  {
                    b
                  }:
                },
              ]
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_on_mutation_in_same_subgraph() {
    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type Mutation {
              update1: T
              update2: T
            }

            type T @key(fields: "id") {
              id: ID!
              v0: String
              v1: String
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              v2: String
            }
          "#,
    );

    // What matters here is that the updates (that go to different fields) are correctly done in sequence,
    // and that defers have proper dependency set.
    assert_plan!(planner,
        r#"
            mutation mut {
              update1 {
                v0
                ... @defer {
                  v1
                }
              }
              update2 {
                v1
                ... @defer {
                  v0
                  v2
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            update1 {
              v0
            }
            update2 {
              v1
            }
          }:
          Fetch(service: "Subgraph1", id: 2) {
            {
              update1 {
                __typename
                v0
                id
              }
              update2 {
                __typename
                v1
                id
              }
            }
          },
        }, [
          Deferred(depends: [2], path: "update1") {
            {
              v1
            }:
            Flatten(path: "update1") {
              Fetch(service: "Subgraph1") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    v1
                  }
                }
              },
            },
          },
          Deferred(depends: [2], path: "update2") {
            {
              v0
              v2
            }:
            Parallel {
              Flatten(path: "update2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      v0
                    }
                  }
                },
              },
              Flatten(path: "update2") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on T {
                      __typename
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
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_on_mutation_on_different_subgraphs() {
    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type Mutation {
              update1: T
            }

            type T @key(fields: "id") {
              id: ID!
              v0: String
              v1: String
            }
          "#,
          Subgraph2: r#"
            type Mutation {
              update2: T
            }

            type T @key(fields: "id") {
              id: ID!
              v2: String
            }
          "#,
    );

    // What matters here is that the updates (that go to different fields) are correctly done in sequence,
    // and that defers have proper dependency set.
    assert_plan!(planner,
        r#"
            mutation mut {
              update1 {
                v0
                ... @defer {
                  v1
                }
              }
              update2 {
                v1
                ... @defer {
                  v0
                  v2
                }
              }
            }
        "#,
        @r###"
          QueryPlan {
            Defer {
              Primary {
                {
                  update1 {
                    v0
                  }
                  update2 {
                    v1
                  }
                }:
                Sequence {
                  Fetch(service: "Subgraph1", id: 0) {
                    {
                      update1 {
                        __typename
                        v0
                        id
                      }
                    }
                  },
                  Fetch(service: "Subgraph2", id: 1) {
                    {
                      update2 {
                        __typename
                        id
                      }
                    }
                  },
                  Flatten(path: "update2") {
                    Fetch(service: "Subgraph1") {
                      {
                        ... on T {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on T {
                          v1
                        }
                      }
                    },
                  },
                },
              }, [
                Deferred(depends: [0], path: "update1") {
                  {
                    v1
                  }:
                  Flatten(path: "update1") {
                    Fetch(service: "Subgraph1") {
                      {
                        ... on T {
                          __typename
                          id
                        }
                      } =>
                      {
                        ... on T {
                          v1
                        }
                      }
                    },
                  },
                },
                Deferred(depends: [1], path: "update2") {
                  {
                    v0
                    v2
                  }:
                  Parallel {
                    Flatten(path: "update2") {
                      Fetch(service: "Subgraph1") {
                        {
                          ... on T {
                            __typename
                            id
                          }
                        } =>
                        {
                          ... on T {
                            v0
                          }
                        }
                      },
                    },
                    Flatten(path: "update2") {
                      Fetch(service: "Subgraph2") {
                        {
                          ... on T {
                            __typename
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
                  },
                },
              ]
            },
          }
        "###
    );
}

// TODO(@TylerBloom): This test fails do to an suboptimal node at the end of the query plan. The
// actual final node is a `Parallel` node that has two identical `Flatten(Fetch)` nodes that
// flatten to expect final node.
#[test]
#[should_panic(expected = "snapshot assertion")]
fn defer_test_defer_on_multi_dependency_deferred_section() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id0") {
            id0: ID!
            v1: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id0") @key(fields: "id1") {
            id0: ID!
            id1: ID!
            v2: Int
          }
        "#,
        Subgraph3: r#"
          type T @key(fields: "id0") @key(fields: "id2") {
            id0: ID!
            id2: ID!
            v3: Int
          }
        "#,
        Subgraph4: r#"
          type T @key(fields: "id1 id2") {
            id1: ID!
            id2: ID!
            v4: Int
          }
        "#,
    );

    assert_plan!(&planner,
        r#"
          {
            t {
              v1
              v2
              v3
              ... @defer {
                v4
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Defer {
            Primary {
              {
                t {
                  v1
                  v2
                  v3
                }
              }:
              Sequence {
                Fetch(service: "Subgraph1") {
                  {
                    t {
                      __typename
                      id0
                      v1
                    }
                  }
                },
                Parallel {
                  Flatten(path: "t") {
                    Fetch(service: "Subgraph2", id: 0) {
                      {
                        ... on T {
                          __typename
                          id0
                        }
                      } =>
                      {
                        ... on T {
                          v2
                          id1
                        }
                      }
                    },
                  },
                  Flatten(path: "t") {
                    Fetch(service: "Subgraph3", id: 1) {
                      {
                        ... on T {
                          __typename
                          id0
                        }
                      } =>
                      {
                        ... on T {
                          v3
                          id2
                        }
                      }
                    },
                  },
                },
              },
            }, [
              Deferred(depends: [0, 1], path: "t") {
                {
                  v4
                }:
                Flatten(path: "t") {
                  Fetch(service: "Subgraph4") {
                    {
                      ... on T {
                        __typename
                        id1
                        id2
                      }
                    } =>
                    {
                      ... on T {
                        v4
                      }
                    }
                  },
                }
              },
            ]
          },
        }
        "###
    );

    // TODO: the following plan is admittedly not as effecient as it could be, as the 2 queries to
    // subgraph 2 and 3 are done in the "primary" section, but all they do is handle transitive
    // key dependencies for the deferred block, so it would make more sense to defer those fetches
    // as well. It is however tricky to both improve this here _and_ maintain the plan generate
    // just above (which is admittedly optimial). More precisely, what the code currently does is
    // that when it gets to a defer, then it defers the fetch that gets the deferred fields (the
    // fetch to subgraph 4 here), but it puts the "condition" resolution for the key of that fetch
    // in the non-deferred section. Here, resolving that fetch conditions is what creates the
    // dependency on the the fetches to subgraph 2 and 3, and so those get non-deferred.
    // Now, it would be reasonably simple to say that when we resolve the "conditions" for a deferred
    // fetch, then the first "hop" is non-deferred, but any following ones do get deferred, which
    // would move the 2 fetches to subgraph 2 and 3 in the deferred section. The problem is that doing
    // that wholesale means that in the previous example above, we'd keep the 2 non-deferred fetches
    // to subgraph 2 and 3 for v2 and v3, but we would then have new deferred fetches to those
    // subgraphs in the deferred section to now get the key id1 and id2, and that is in turn arguably
    // non-optimal. So ideally, the code would be able to distinguish between those 2 cases and
    // do the most optimal thing in each cases, but it's not that simple to do with the current
    // code.
    // Taking a step back, this "inefficiency" only exists where there is a @key "chain", and while
    // such chains have their uses, they are likely pretty rare in the first place. And as the
    // generated plan is not _that_ bad either, optimizing this feels fairly low priority and
    // we leave it for "later".
    assert_plan!(planner,
        r#"
          {
            t {
              v1
              ... @defer {
                v4
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Defer {
            Primary {
              {
                t {
                  v1
                }
              }:
              Sequence {
                Fetch(service: "Subgraph1") {
                  {
                    t {
                      __typename
                      v1
                      id0
                    }
                  }
                },
                Parallel {
                  Flatten(path: "t") {
                    Fetch(service: "Subgraph3", id: 0) {
                      {
                        ... on T {
                          __typename
                          id0
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
                    Fetch(service: "Subgraph2", id: 1) {
                      {
                        ... on T {
                          __typename
                          id0
                        }
                      } =>
                      {
                        ... on T {
                          id1
                        }
                      }
                    },
                  },
                },
              }
            }, [
              Deferred(depends: [0, 1], path: "t") {
                {
                  v4
                }:
                Flatten(path: "t") {
                  Fetch(service: "Subgraph4") {
                    {
                      ... on T {
                        __typename
                        id1
                        id2
                      }
                    } =>
                    {
                      ... on T {
                        v4
                      }
                    }
                  },
                }
              },
            ]
          },
        }
        "###
    );
}

#[test]
fn defer_test_requirements_of_deferred_fields_are_deferred() {
    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              v1: Int
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              v2: Int @requires(fields: "v3")
              v3: Int @external
            }
          "#,
          Subgraph3: r#"
            type T @key(fields: "id") {
              id: ID!
              v3: Int
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                v1
                ... @defer {
                  v2
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v1
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                v1
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              v2
            }:
            Sequence {
              Flatten(path: "t") {
                Fetch(service: "Subgraph3") {
                  {
                    ... on T {
                      __typename
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
                Fetch(service: "Subgraph2") {
                  {
                    ... on T {
                      __typename
                      v3
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
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_provides_are_ignored_for_deferred_fields() {
    // NOTE: this test tests the currently implemented behaviour, which ignore @provides when it
    // concerns a deferred field. However, this is the behaviour implemented at the moment more
    // because it is the simplest option and it's not illogical, but it is not the only possibly
    // valid option. In particular, one could make the case that if a subgraph has a `@provides`,
    // then this probably means that the subgraph can provide the field "cheaply" (why have
    // a `@provides` otherwise?), and so that ignoring the @defer (instead of ignoring the @provides)
    // is preferable. We can change to this behaviour later if we decide that it is preferable since
    // the responses sent to the end-user would be the same regardless.

    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T @provides(fields: "v2")
            }

            type T @key(fields: "id") {
              id: ID!
              v1: Int
              v2: Int @external
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              v2: Int @shareable
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                v1
                ... @defer {
                  v2
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              v1
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                v1
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              v2
            }:
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
                    v2
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_on_query_root_type() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            op1: Int
            op2: A
          }

          type A {
            x: Int
            y: Int
            next: Query
          }
        "#,
        Subgraph2: r#"
          type Query {
            op3: Int
            op4: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            op2 {
              x
              y
              next {
                op3
                ... @defer {
                  op1
                  op4
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            op2 {
              x
              y
              next {
                op3
              }
            }
          }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                op2 {
                  x
                  y
                  next {
                    __typename
                  }
                }
              }
            },
            Flatten(path: "op2.next") {
              Fetch(service: "Subgraph2") {
                {
                  ... on Query {
                    op3
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "op2/next") {
            {
              op1
              op4
            }:
            Parallel {
              Flatten(path: "op2.next") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Query {
                      op1
                    }
                  }
                },
              },
              Flatten(path: "op2.next") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on Query {
                      op4
                    }
                  }
                },
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_on_everything_queried() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            y: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            ... @defer {
              t {
                x
                y
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {}, [
          Deferred(depends: [], path: "") {
            {
              t {
                x
                y
              }
            }:
            Sequence {
              Flatten(path: "") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Query {
                      t {
                        __typename
                        id
                        x
                      }
                    }
                  }
                },
              },
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
                      y
                    }
                  }
                },
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_everything_within_entity() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            y: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              ... @defer {
                x
                y
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              x
              y
            }:
            Parallel {
              Flatten(path: "t") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      x
                    }
                  }
                },
              },
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
                      y
                    }
                  }
                },
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_with_conditions_and_labels() {
    let planner = planner!(config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              x: Int
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              y: Int
            }
          "#,
    );

    // without explicit label
    assert_plan!(&planner,
        r#"
          query($cond: Boolean) {
            t {
              x
              ... @defer(if: $cond) {
                y
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Condition(if: $cond) {
        Then {
          Defer {
            Primary {
              {
                t {
                  x
                }
              }:
              Fetch(service: "Subgraph1", id: 0) {
                {
                  t {
                    __typename
                    x
                    id
                  }
                }
              },
            }, [
              Deferred(depends: [0], path: "t") {
                {
                  y
                }:
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
                        y
                      }
                    }
                  },
                },
              },
            ]
          },
        } Else {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                t {
                  __typename
                  id
                  x
                }
              }
            },
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
                    y
                  }
                }
              },
            },
          },
        },
      },
    }
    "###
    );
    // with explicit label
    assert_plan!(planner,
        r#"
          query($cond: Boolean) {
            t {
              x
              ... @defer(label: "testLabel" if: $cond) {
                y
              }
            }
          }
        "#,
        @r###"
          QueryPlan {
            Condition(if: $cond) {
              Then {
                Defer {
                  Primary {
                    {
                      t {
                        x
                      }
                    }:
                    Fetch(service: "Subgraph1", id: 0) {
                      {
                        t {
                          __typename
                          x
                          id
                        }
                      }
                    },
                  }, [
                    Deferred(depends: [0], path: "t", label: "testLabel") {
                      {
                        y
                      }:
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
                              y
                            }
                          }
                        },
                      },
                    },
                  ]
                },
              } Else {
                Sequence {
                  Fetch(service: "Subgraph1") {
                    {
                      t {
                        __typename
                        id
                        x
                      }
                    }
                  },
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
                          y
                        }
                      }
                    },
                  },
                },
              },
            },
          }
        "###
    );
}

#[test]
fn defer_test_defer_with_condition_on_single_subgraph() {
    // This test mostly serves to illustrate why we handle @defer conditions with `ConditionNode` instead of
    // just generating only the plan with the @defer and ignoring the `DeferNode` at execution: this is
    // because doing can result in sub-par execution for the case where the @defer is disabled (unless of
    // course the execution "merges" fetch groups, but it's not trivial to do so).

    let planner = planner!(config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              x: Int
              y: Int
            }
          "#,
    );
    assert_plan!(planner,
        r#"
            query ($cond: Boolean) {
              t {
                x
                ... @defer(if: $cond) {
                  y
                }
              }
            }
        "#,
        @r###"
    QueryPlan {
      Condition(if: $cond) {
        Then {
          Defer {
            Primary {
              {
                t {
                  x
                }
              }:
              Fetch(service: "Subgraph1", id: 0) {
                {
                  t {
                    __typename
                    x
                    id
                  }
                }
              },
            }, [
              Deferred(depends: [0], path: "t") {
                {
                  y
                }:
                Flatten(path: "t") {
                  Fetch(service: "Subgraph1") {
                    {
                      ... on T {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on T {
                        y
                      }
                    }
                  },
                },
              },
            ]
          },
        } Else {
          Fetch(service: "Subgraph1") {
            {
              t {
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

#[test]
fn defer_test_defer_with_mutliple_conditions_and_labels() {
    let planner = planner!(config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              x: Int
              u: U
            }

            type U @key(fields: "id") {
              id: ID!
              a: Int
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              y: Int
            }
          "#,
          Subgraph3: r#"
            type U @key(fields: "id") {
              id: ID!
              b: Int
            }
          "#,
    );
    assert_plan!(planner,
        r#"
            query ($cond1: Boolean, $cond2: Boolean) {
              t {
                x
                ... @defer(if: $cond1, label: "foo") {
                  y
                }
                ... @defer(if: $cond2, label: "bar") {
                  u {
                    a
                    ... @defer(if: $cond1) {
                      b
                    }
                  }
                }
              }
            }
        "#,
        @r###"
          QueryPlan {
            Condition(if: $cond1) {
              Then {
                Condition(if: $cond2) {
                  Then {
                    Defer {
                      Primary {
                        {
                          t {
                            x
                          }
                        }:
                        Fetch(service: "Subgraph1", id: 0) {
                          {
                            t {
                              __typename
                              x
                              id
                            }
                          }
                        },
                      }, [
                        Deferred(depends: [0], path: "t", label: "bar") {
                          Defer {
                            Primary {
                              {
                                u {
                                  a
                                }
                              }:
                              Flatten(path: "t") {
                                Fetch(service: "Subgraph1", id: 1) {
                                  {
                                    ... on T {
                                      __typename
                                      id
                                    }
                                  } =>
                                  {
                                    ... on T {
                                      u {
                                        __typename
                                        a
                                        id
                                      }
                                    }
                                  }
                                },
                              },
                            }, [
                              Deferred(depends: [1], path: "t/u") {
                                {
                                  b
                                }:
                                Flatten(path: "t.u") {
                                  Fetch(service: "Subgraph3") {
                                    {
                                      ... on U {
                                        __typename
                                        id
                                      }
                                    } =>
                                    {
                                      ... on U {
                                        b
                                      }
                                    }
                                  },
                                },
                              },
                            ]
                          },
                        },
                        Deferred(depends: [0], path: "t", label: "foo") {
                          {
                            y
                          }:
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
                                  y
                                }
                              }
                            },
                          },
                        },
                      ]
                    },
                  } Else {
                    Defer {
                      Primary {
                        {
                          t {
                            x
                            u {
                              a
                            }
                          }
                        }:
                        Fetch(service: "Subgraph1", id: 2) {
                          {
                            t {
                              __typename
                              x
                              id
                              u {
                                __typename
                                a
                                id
                              }
                            }
                          }
                        },
                      }, [
                        Deferred(depends: [2], path: "t", label: "foo") {
                          {
                            y
                          }:
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
                                  y
                                }
                              }
                            },
                          },
                        },
                        Deferred(depends: [2], path: "t/u") {
                          {
                            b
                          }:
                          Flatten(path: "t.u") {
                            Fetch(service: "Subgraph3") {
                              {
                                ... on U {
                                  __typename
                                  id
                                }
                              } =>
                              {
                                ... on U {
                                  b
                                }
                              }
                            },
                          },
                        },
                      ]
                    },
                  },
                },
              } Else {
                Condition(if: $cond2) {
                  Then {
                    Defer {
                      Primary {
                        {
                          t {
                            x
                            y
                          }
                        }:
                        Sequence {
                          Fetch(service: "Subgraph1", id: 3) {
                            {
                              t {
                                __typename
                                id
                                x
                              }
                            }
                          },
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
                                  y
                                }
                              }
                            },
                          },
                        },
                      }, [
                        Deferred(depends: [3], path: "t", label: "bar") {
                          {
                            u {
                              a
                              b
                            }
                          }:
                          Sequence {
                            Flatten(path: "t") {
                              Fetch(service: "Subgraph1") {
                                {
                                  ... on T {
                                    __typename
                                    id
                                  }
                                } =>
                                {
                                  ... on T {
                                    u {
                                      __typename
                                      id
                                      a
                                    }
                                  }
                                }
                              },
                            },
                            Flatten(path: "t.u") {
                              Fetch(service: "Subgraph3") {
                                {
                                  ... on U {
                                    __typename
                                    id
                                  }
                                } =>
                                {
                                  ... on U {
                                    b
                                  }
                                }
                              },
                            },
                          },
                        },
                      ]
                    },
                  } Else {
                    Sequence {
                      Fetch(service: "Subgraph1") {
                        {
                          t {
                            __typename
                            id
                            x
                            u {
                              __typename
                              id
                              a
                            }
                          }
                        }
                      },
                      Parallel {
                        Flatten(path: "t.u") {
                          Fetch(service: "Subgraph3") {
                            {
                              ... on U {
                                __typename
                                id
                              }
                            } =>
                            {
                              ... on U {
                                b
                              }
                            }
                          },
                        },
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
                                y
                              }
                            }
                          },
                        },
                      },
                    },
                  },
                },
              },
            },
          }
        "###
    );
}

#[test]
fn defer_test_interface_has_different_definitions_between_subgraphs() {
    // This test exists to ensure an early bug is fixed: that bug was in the code building
    // the `subselection` of `DeferNode` in the plan, and was such that those subselections
    // were created with links to subgraph types instead the supergraph ones. As a result,
    // we were sometimes trying to add a field (`b` in the example here) to version of a
    // type that didn't had that field (the definition of `I` in Subgraph1 here), hence
    // running into an assertion error.

    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            i: I
          }

          interface I {
            a: Int
            c: Int
          }

          type T implements I @key(fields: "id") {
            id: ID!
            a: Int
            c: Int
          }
        "#,
        Subgraph2: r#"
          interface I {
            b: Int
          }

          type T implements I @key(fields: "id") {
            id: ID!
            a: Int @external
            b: Int @requires(fields: "a")
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          query Dimensions {
            i {
              a
              b
              ... @defer {
                c
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            i {
              a
              ... on T {
                b
              }
            }
          }:
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                i {
                  __typename
                  a
                  ... on T {
                    __typename
                    id
                    a
                  }
                  c
                }
              }
            },
            Flatten(path: "i") {
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
        }, [
          Deferred(depends: [], path: "i") {
            {
              c
            }:
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_named_fragments_simple() {
    let planner = planner!(
        config = config_with_defer(),
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
              x: Int
              y: Int
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                ...TestFragment @defer
              }
            }

            fragment TestFragment on T {
              x
              y
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              ... on T {
                x
                y
              }
            }:
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
                    x
                    y
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_fragments_expand_into_same_field_regardless_of_defer() {
    let planner = planner!(
        config = config_with_defer(),
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
              x: Int
              y: Int
              z: Int
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                ...Fragment1
                ...Fragment2 @defer
              }
            }

            fragment Fragment1 on T {
              x
              y
            }

            fragment Fragment2 on T {
              y
              z
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              x
              y
            }
          }:
          Sequence {
            Fetch(service: "Subgraph1", id: 0) {
              {
                t {
                  __typename
                  id
                }
              }
            },
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
                    x
                    y
                  }
                }
              },
            },
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              ... on T {
                y
                z
              }
            }:
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
                    y
                    z
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_can_request_typename_in_fragment() {
    // NOTE: There is nothing super special about __typename in theory, but because it's a field
    // that is always available in all subghraph (for a type the subgraph has), it tends to create
    // multiple options for the query planner, and so excercises some code-paths that triggered an
    // early bug in the handling of `@defer`
    // (https://github.com/apollographql/federation/issues/2128).
    let planner = planner!(
        config = config_with_defer(),
          Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              x: Int
            }
          "#,
          Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              y: Int
            }
          "#,
    );

    assert_plan!(planner,
        r#"
            {
              t {
                ...OnT @defer
                x
              }
            }

            fragment OnT on T {
              y
              __typename
            }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              x
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                id
                x
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              ... on T {
                __typename
                y
              }
            }:
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
                    __typename
                    y
                  }
                }
              },
            },
          },
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_do_not_merge_query_branches_with_defer() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: Int
            b: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            c: Int
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              a
              ... @defer {
                b
              }
              ... @defer {
                c
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            t {
              a
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              t {
                __typename
                a
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "t") {
            {
              c
            }:
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
                    c
                  }
                }
              },
            },
          },
          Deferred(depends: [0], path: "t") {
            {
              b
            }:
            Flatten(path: "t") {
              Fetch(service: "Subgraph1") {
                {
                  ... on T {
                    __typename
                    id
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
        ]
      },
    }
    "###
    );
}

#[test]
fn defer_test_defer_only_the_key_of_an_entity() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: String
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            t {
              v0
              ... @defer {
                id
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Defer {
            Primary {
              {
                t {
                  v0
                }
              }:
              Fetch(service: "Subgraph1") {
                {
                  t {
                    v0
                    id
                  }
                }
              },
            }, [
              Deferred(depends: [], path: "t") {
                {
                  id
                }:
              },
            ]
          },
        }
        "###
    );
}

#[test]
fn defer_test_the_path_in_defer_includes_traversed_fragments() {
    let planner = planner!(
        config = config_with_defer(),
        Subgraph1: r#"
          type Query {
            i: I
          }

          interface I {
            x: Int
          }

          type A implements I {
            x: Int
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            v1: String
            v2: String
          }
        "#,
    );

    assert_plan!(planner,
        r#"
          {
            i {
              ... on A {
                t {
                  v1
                  ... @defer {
                    v2
                  }
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Defer {
        Primary {
          {
            i {
              ... on A {
                t {
                  v1
                }
              }
            }
          }:
          Fetch(service: "Subgraph1", id: 0) {
            {
              i {
                __typename
                ... on A {
                  t {
                    __typename
                    v1
                    id
                  }
                }
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "i/... on A/t") {
            {
              v2
            }:
            Flatten(path: "i.t") {
              Fetch(service: "Subgraph1") {
                {
                  ... on T {
                    __typename
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
          },
        ]
      },
    }
    "###
    );
}
