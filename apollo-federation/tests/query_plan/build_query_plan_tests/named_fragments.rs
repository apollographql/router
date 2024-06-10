#[test]
fn handles_mix_of_fragments_indirection_and_unions() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            parent: Parent
          }

          union CatOrPerson = Cat | Parent | Child

          type Parent {
            childs: [Child]
          }

          type Child {
            id: ID!
          }

          type Cat {
            name: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            parent {
              ...F_indirection1_parent
            }
          }

          fragment F_indirection1_parent on Parent {
            ...F_indirection2_catOrPerson
          }

          fragment F_indirection2_catOrPerson on CatOrPerson {
            ...F_catOrPerson
          }

          fragment F_catOrPerson on CatOrPerson {
            __typename
            ... on Cat {
              name
            }
            ... on Parent {
              childs {
                __typename
                id
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              parent {
                __typename
                childs {
                  __typename
                  id
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
// #[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn another_mix_of_fragments_indirection_and_unions() {
    // This tests that the issue reported on https://github.com/apollographql/router/issues/3172 is resolved.

    let planner = planner!(
        Subgraph1: r#"
          type Query {
            owner: Owner!
          }

          interface OItf {
            id: ID!
            v0: String!
          }

          type Owner implements OItf {
            id: ID!
            v0: String!
            u: [U]
          }

          union U = T1 | T2

          interface I {
            id1: ID!
            id2: ID!
          }

          type T1 implements I {
            id1: ID!
            id2: ID!
            owner: Owner!
          }

          type T2 implements I {
            id1: ID!
            id2: ID!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            owner {
              u {
                ... on I {
                  id1
                  id2
                }
                ...Fragment1
                ...Fragment2
              }
            }
          }

          fragment Fragment1 on T1 {
            owner {
              ... on Owner {
                ...Fragment3
              }
            }
          }

          fragment Fragment2 on T2 {
            ...Fragment4
            id1
          }

          fragment Fragment3 on OItf {
            v0
          }

          fragment Fragment4 on I {
            id1
            id2
            __typename
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              owner {
                u {
                  __typename
                  ...Fragment4
                  ... on T1 {
                    owner {
                      v0
                    }
                  }
                  ... on T2 {
                    ...Fragment4
                  }
                }
              }
            }

            fragment Fragment4 on I {
              __typename
              id1
              id2
            }
          },
        }
      "###
    );

    assert_plan!(
        &planner,
        r#"
          {
            owner {
              u {
                ... on I {
                  id1
                  id2
                }
                ...Fragment1
                ...Fragment2
              }
            }
          }

          fragment Fragment1 on T1 {
            owner {
              ... on Owner {
                ...Fragment3
              }
            }
          }

          fragment Fragment2 on T2 {
            ...Fragment4
            id1
          }

          fragment Fragment3 on OItf {
            v0
          }

          fragment Fragment4 on I {
            id1
            id2
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              owner {
                u {
                  __typename
                  ... on I {
                    __typename
                    ...Fragment4
                  }
                  ... on T1 {
                    owner {
                      v0
                    }
                  }
                  ... on T2 {
                    ...Fragment4
                  }
                }
              }
            }

            fragment Fragment4 on I {
              id1
              id2
            }
          },
        }
      "###
    );
}

#[test]
// TODO: investigate this failure
fn handles_fragments_with_interface_field_subtyping() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t1: T1!
          }

          interface I {
            id: ID!
            other: I!
          }

          type T1 implements I {
            id: ID!
            other: T1!
          }

          type T2 implements I {
            id: ID!
            other: T2!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t1 {
              ...Fragment1
            }
          }

          fragment Fragment1 on I {
            other {
              ... on T1 {
                id
              }
              ... on T2 {
                id
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t1 {
                other {
                  __typename
                  id
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn can_reuse_fragments_in_subgraph_where_they_only_partially_apply_in_root_fetch() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t1: T
            t2: T
          }

          type T @key(fields: "id") {
            id: ID!
            v0: Int
            v1: Int
            v2: Int
          }
        "#,
        Subgraph2: r#"
          type T @key(fields: "id") {
            id: ID!
            v3: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t1 {
              ...allTFields
            }
            t2 {
              ...allTFields
            }
          }

          fragment allTFields on T {
            v0
            v1
            v2
            v3
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                t1 {
                  __typename
                  ...allTFields
                  id
                }
                t2 {
                  __typename
                  ...allTFields
                  id
                }
              }

              fragment allTFields on T {
                v0
                v1
                v2
              }
            },
            Parallel {
              Flatten(path: "t2") {
                Fetch(service: "Subgraph2") {
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
              Flatten(path: "t1") {
                Fetch(service: "Subgraph2") {
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
        }
      "###
    );
}

#[test]
fn can_reuse_fragments_in_subgraph_where_they_only_partially_apply_in_entity_fetch() {
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
            u1: U
            u2: U
          }

          type U @key(fields: "id") {
            id: ID!
            v0: Int
            v1: Int
          }
        "#,
        Subgraph3: r#"
          type U @key(fields: "id") {
            id: ID!
            v2: Int
            v3: Int
          }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              u1 {
                ...allUFields
              }
              u2 {
                ...allUFields
              }
            }
          }

          fragment allUFields on U {
            v0
            v1
            v2
            v3
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
                    u1 {
                      __typename
                      ...allUFields
                      id
                    }
                    u2 {
                      __typename
                      ...allUFields
                      id
                    }
                  }
                }

                fragment allUFields on U {
                  v0
                  v1
                }
              },
            },
            Parallel {
              Flatten(path: "t.u2") {
                Fetch(service: "Subgraph3") {
                  {
                    ... on U {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on U {
                      v2
                      v3
                    }
                  }
                },
              },
              Flatten(path: "t.u1") {
                Fetch(service: "Subgraph3") {
                  {
                    ... on U {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on U {
                      v2
                      v3
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
