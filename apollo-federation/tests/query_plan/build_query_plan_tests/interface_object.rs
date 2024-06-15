use std::ops::Deref;

use apollo_compiler::NodeStr;
use apollo_federation::query_plan::FetchDataPathElement;
use apollo_federation::query_plan::FetchDataRewrite;

use crate::query_plan::build_query_plan_support::find_fetch_nodes_for_subgraph;

const SUBGRAPH1: &str = r#"
  type Query {
    iFromS1: I
  }

  interface I @key(fields: "id") {
    id: ID!
    x: Int
  }

  type A implements I @key(fields: "id") {
    id: ID!
    x: Int
    z: Int
  }

  type B implements I @key(fields: "id") {
    id: ID!
    x: Int
    w: Int
  }
"#;

const SUBGRAPH2: &str = r#"
  type Query {
    iFromS2: I
  }

  type I @interfaceObject @key(fields: "id") {
    id: ID!
    y: Int
  }
"#;

#[test]
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure (fetch node for `iFromS1.y` is missing)
fn can_use_a_key_on_an_interface_object_type() {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    assert_plan!(
        &planner,
        r#"
          {
            iFromS1 {
              x
              y
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S1") {
                {
                  iFromS1 {
                    __typename
                    id
                    x
                  }
                }
              },
              Flatten(path: "iFromS1") {
                Fetch(service: "S2") {
                  {
                    ... on I {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
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
fn can_use_a_key_on_an_interface_object_from_an_interface_object_type() {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    assert_plan!(
        &planner,
        r#"
          {
            iFromS2 {
              x
              y
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S2") {
                {
                  iFromS2 {
                    __typename
                    id
                    y
                  }
                }
              },
              Flatten(path: "iFromS2") {
                Fetch(service: "S1") {
                  {
                    ... on I {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
                      __typename
                      x
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
fn only_uses_an_interface_object_if_it_can() {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    assert_plan!(
        &planner,
        r#"
        {
          iFromS2 {
            y
          }
        }
        "#,

        @r###"
          QueryPlan {
            Fetch(service: "S2") {
              {
                iFromS2 {
                  y
                }
              }
            },
          }
      "###
    );
}

#[test]
fn does_not_rely_on_an_interface_object_directly_for_typename() {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    assert_plan!(
        &planner,
        r#"
        {
          iFromS2 {
            __typename
            y
          }
        }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S2") {
                {
                  iFromS2 {
                    __typename
                    id
                    y
                  }
                }
              },
              Flatten(path: "iFromS2") {
                Fetch(service: "S1") {
                  {
                    ... on I {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
                      __typename
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
#[should_panic(
    expected = r#"Cannot add selection of field "I.id" to selection set of parent type "A""#
)]
// TODO: investigate this failure
// - Fails to rebase on an interface object type in a subgraph.
fn does_not_rely_on_an_interface_object_directly_if_a_specific_implementation_is_requested() {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    // Even though `y` is part of the interface and accessible from the 2nd subgraph, the
    // fact that we "filter" a single implementation should act as if `__typename` was queried
    // (effectively, the gateway/router need that `__typename` to decide if the returned data
    // should be included or not.
    assert_plan!(
        &planner,
        r#"
        {
          iFromS2 {
            ... on A {
              y
            }
          }
        }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S2") {
                {
                  iFromS2 {
                    __typename
                    id
                  }
                }
              },
              Flatten(path: "iFromS2") {
                Fetch(service: "S1") {
                  {
                    ... on I {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
                      __typename
                    }
                  }
                },
              },
              Flatten(path: "iFromS2") {
                Fetch(service: "S2") {
                  {
                    ... on A {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
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
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure (fetch node for `iFromS1.y` is missing)
fn can_use_a_key_on_an_interface_object_type_even_for_a_concrete_implementation() {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          iFromS1 {
            ... on A {
              y
            }
          }
        }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S1") {
                {
                  iFromS1 {
                    __typename
                    ... on A {
                      __typename
                      id
                    }
                  }
                }
              },
              Flatten(path: "iFromS1") {
                Fetch(service: "S2") {
                  {
                    ... on A {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
                      y
                    }
                  }
                },
              },
            },
          }
      "###
    );

    let fetch_nodes = find_fetch_nodes_for_subgraph("S2", &plan);
    assert_eq!(fetch_nodes.len(), 1);
    let rewrites = fetch_nodes[0].input_rewrites.clone();
    assert_eq!(rewrites.len(), 1);
    let rewrite = rewrites[0].clone();
    match rewrite.deref() {
        FetchDataRewrite::ValueSetter(v) => {
            assert_eq!(v.path.len(), 1);
            match &v.path[0] {
                FetchDataPathElement::TypenameEquals(typename) => {
                    assert_eq!(typename, &NodeStr::new("A"))
                }
                _ => unreachable!("Expected FetchDataPathElement::TypenameEquals path"),
            }
            assert_eq!(v.set_value_to, "I");
        }
        _ => unreachable!("Expected FetchDataRewrite::ValueSetter rewrite"),
    }
}

#[test]
fn handles_query_of_an_interface_field_for_a_specific_implementation_when_query_starts_with_interface_object(
) {
    let planner = planner!(
        S1: SUBGRAPH1,
        S2: SUBGRAPH2,
    );
    // Here, we start on S2, but `x` is only in S1. Further, while `x` is on the `I` interface, we only query it for `A`.
    assert_plan!(
        &planner,
        r#"
        {
          iFromS2 {
            ... on A {
              x
            }
          }
        }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S2") {
                {
                  iFromS2 {
                    __typename
                    id
                  }
                }
              },
              Flatten(path: "iFromS2") {
                Fetch(service: "S1") {
                  {
                    ... on I {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on I {
                      __typename
                      ... on A {
                        x
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
#[should_panic(
    expected = r#"Cannot add selection of field "I.id" to selection set of parent type "A""#
)]
// TODO: investigate this failure
// - Fails to rebase on an interface object type in a subgraph.
fn it_avoids_buffering_interface_object_results_that_may_have_to_be_filtered_with_lists() {
    let planner = planner!(
        S1: r#"
          type Query {
            everything: [I]
          }

          type I @interfaceObject @key(fields: "id") {
            id: ID!
            expansiveField: String
          }
        "#,
        S2: r#"
          interface I @key(fields: "id") {
            id: ID!
          }

          type A implements I @key(fields: "id") {
            id: ID!
            a: Int
          }

          type B implements I @key(fields: "id") {
            id: ID!
            b: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            everything {
              ... on A {
                a
                expansiveField
              }
            }
          }
        "#,

        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "S1") {
              {
                everything {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "everything.@") {
              Fetch(service: "S2") {
                {
                  ... on I {
                    __typename
                    id
                  }
                } =>
                {
                  ... on I {
                    __typename
                    ... on A {
                      a
                    }
                  }
                }
              },
            },
            Flatten(path: "everything.@") {
              Fetch(service: "S1") {
                {
                  ... on A {
                    __typename
                    id
                  }
                } =>
                {
                  ... on I {
                    expansiveField
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
// TODO: investigate this failure (missing fetch nodes for i.x and i.y)
fn it_handles_requires_on_concrete_type_of_field_provided_by_interface_object() {
    let planner = planner!(
        S1: r#"
          type I @interfaceObject @key(fields: "id") {
            id: ID!
            x: Int @shareable
          }
        "#,
        S2: r#"
          type Query {
            i: I
          }

          interface I @key(fields: "id") {
            id: ID!
            x: Int
          }

          type A implements I @key(fields: "id") {
            id: ID!
            x: Int @external
            y: String @requires(fields: "x")
          }

          type B implements I @key(fields: "id") {
            id: ID!
            x: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i {
              ... on A {
                y
              }
            }
          }
        "#,

        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "S2") {
              {
                i {
                  __typename
                  ... on A {
                    __typename
                    id
                  }
                }
              }
            },
            Flatten(path: "i") {
              Fetch(service: "S1") {
                {
                  ... on A {
                    __typename
                    id
                  }
                } =>
                {
                  ... on I {
                    x
                  }
                }
              },
            },
            Flatten(path: "i") {
              Fetch(service: "S2") {
                {
                  ... on A {
                    __typename
                    x
                    id
                  }
                } =>
                {
                  ... on A {
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
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure (missing fetch node for `i.t.relatedIs.id`)
fn it_handles_interface_object_in_nested_entity() {
    let planner = planner!(
        S1: r#"
          type I @interfaceObject @key(fields: "id") {
            id: ID!
            t: T
          }

          type T {
            relatedIs: [I]
          }
        "#,
        S2: r#"
          type Query {
            i: I
          }

          interface I @key(fields: "id") {
            id: ID!
            a: Int
          }

          type A implements I @key(fields: "id") {
            id: ID!
            a: Int
          }

          type B implements I @key(fields: "id") {
            id: ID!
            a: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i {
              t {
                relatedIs {
                  a
                }
              }
            }
          }
        "#,

        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "S2") {
              {
                i {
                  __typename
                  id
                }
              }
            },
            Flatten(path: "i") {
              Fetch(service: "S1") {
                {
                  ... on I {
                    __typename
                    id
                  }
                } =>
                {
                  ... on I {
                    t {
                      relatedIs {
                        __typename
                        id
                      }
                    }
                  }
                }
              },
            },
            Flatten(path: "i.t.relatedIs.@") {
              Fetch(service: "S2") {
                {
                  ... on I {
                    __typename
                    id
                  }
                } =>
                {
                  ... on I {
                    __typename
                    a
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
fn it_handles_interface_object_input_rewrites_when_cloning_dependency_graph() {
    let planner = planner!(
        S1: r#"
          type Query {
            i: I!
          }

          interface I @key(fields: "i1") {
            i1: String!
            i2: T
          }

          type T @key(fields: "t1", resolvable: false) {
            t1: String!
          }

          type U implements I @key(fields: "i1") {
            id: ID!
            i1: String!
            i2: T @shareable
          }
        "#,
        S2: r#"
          type I @interfaceObject @key(fields: "i1") {
            i1: String!
            i2: T @shareable
            i3: Int
          }

          type T @key(fields: "t1", resolvable: false) {
            t1: String!
          }
        "#,
        S3: r#"
          type T @key(fields: "t1") {
            t1: String!
            t2: String! @shareable
            t3: Int
          }
        "#,
        S4: r#"
          type T @key(fields: "t1") {
            t1: String!
            t2: String! @shareable
            t4: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            i {
              __typename
              i2 {
                __typename
                t2
              }
              i3
            }
          }
        "#,

        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "S1") {
              {
                i {
                  __typename
                  i1
                  i2 {
                    __typename
                    t1
                  }
                }
              }
            },
            Parallel {
              Flatten(path: "i") {
                Fetch(service: "S2") {
                  {
                    ... on I {
                      __typename
                      i1
                    }
                  } =>
                  {
                    ... on I {
                      i3
                    }
                  }
                },
              },
              Flatten(path: "i.i2") {
                Fetch(service: "S3") {
                  {
                    ... on T {
                      __typename
                      t1
                    }
                  } =>
                  {
                    ... on T {
                      __typename
                      t2
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
