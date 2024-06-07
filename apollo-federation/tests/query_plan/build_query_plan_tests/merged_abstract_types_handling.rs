/*!
 * Those tests the cases where 2 abstract types (interface or union) interact (having some common runtime
 * types intersection), but one of them include an runtime type that the other also include _in the supergraph_
 * but *not* in one of the subgraph. The tl;dr is that in some of those interaction, we must force a type-explosion
 * to handle it properly, but no in other interactions, and this ensures this is handled properly.
 */

#[test]
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn union_interface_interaction() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            u: U
          }

          union U = A | B | C

          interface I {
            v: Int
          }

          type A {
            v: Int @shareable
          }

          type B implements I {
            v: Int
          }

          type C implements I {
            v: Int
          }
        "#,
        Subgraph2: r#"
          interface I {
            v: Int
          }

          type A implements I {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u {
              ... on I {
                v
              }
            }
          }
        "#,


      // Type `A` can be returned by `u` and is a `I` *in the supergraph* but not in `Subgraph1`, so need to
      // type-explode `I` in the query to `Subgraph1` so it doesn't exclude `A`.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              u {
                __typename
                ... on A {
                  v
                }
                ... on B {
                  v
                }
                ... on C {
                  v
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn union_interface_interaction_but_no_need_to_type_explode() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            u: U
          }

          union U = B | C

          interface I {
            v: Int
          }

          type A implements I {
            v: Int @shareable
          }

          type B implements I {
            v: Int
          }

          type C implements I {
            v: Int
          }
        "#,
        Subgraph2: r#"
          union U = A

          type A {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u {
              ... on I {
                v
              }
            }
          }
        "#,


      // While `A` is a `U` in the supergraph while not in `Subgraph1`, since the `u`
      // operation is resolved by `Subgraph1`, it cannot ever return a A, and so
      // there is need to type-explode `I` in this query.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              u {
                __typename
                ... on I {
                  __typename
                  v
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn interface_union_interaction() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i: I
          }

          union U = B | C

          interface I {
            v: Int
          }

          type A implements I {
            v: Int @shareable
          }

          type B implements I {
            v: Int
          }

          type C implements I {
            v: Int
          }
        "#,
        Subgraph2: r#"
          union U = A

          type A {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i {
              ... on U {
                ... on A {
                  v
                }
              }
            }
          }
        "#,


      // Type `A` can be returned by `i` and is a `U` *in the supergraph* but not in `Subgraph1`, so need to
      // type-explode `U` in the query to `Subgraph1` so it doesn't exclude `A`.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              i {
                __typename
                ... on A {
                  v
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
#[should_panic(
    expected = r#"Cannot add fragment of condition "A" (runtimes: [A]) to parent type "I" (runtimes: [B, C])"#
)]
// TODO: investigate this failure
fn interface_union_interaction_but_no_need_to_type_explode() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i: I
          }

          union U = A | B | C

          interface I {
            v: Int
          }

          type A {
            v: Int @shareable
          }

          type B implements I {
            v: Int
          }

          type C implements I {
            v: Int
          }
        "#,
        Subgraph2: r#"
          interface I {
            v: Int
          }

          type A implements I {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i {
              ... on U {
                ... on A {
                  v
                }
              }
            }
          }
        "#,


      // Here, `A` is a `I` in the supergraph while not in `Subgraph1`, and since the `i` operation is resolved by
      // `Subgraph1`, it cannot ever return a A. And so we can skip the whole `... on U` sub-selection.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              i {
                __typename
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
fn interface_interface_interaction() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i1: I1
          }

          interface I1 {
            v: Int
          }

          interface I2 {
            v: Int
          }

          type A implements I1 {
            v: Int @shareable
          }

          type B implements I1 & I2 {
            v: Int
          }

          type C implements I1 & I2 {
            v: Int
          }
        "#,
        Subgraph2: r#"
          interface I2 {
            v: Int
          }

          type A implements I2 {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i1 {
              ... on I2 {
                v
              }
            }
          }
        "#,


      // Type `A` can be returned by `i1` and is a `I2` *in the supergraph* but not in `Subgraph1`, so need to
      // type-explode `I2` in the query to `Subgraph1` so it doesn't exclude `A`.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              i1 {
                __typename
                ... on A {
                  v
                }
                ... on B {
                  v
                }
                ... on C {
                  v
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn interface_interface_interaction_but_no_need_to_type_explode() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            i1: I1
          }

          interface I1 {
            v: Int
          }

          interface I2 {
            v: Int
          }

          type A implements I2 {
            v: Int @shareable
          }

          type B implements I1 & I2 {
            v: Int
          }

          type C implements I1 & I2 {
            v: Int
          }
        "#,
        Subgraph2: r#"
          interface I1 {
            v: Int
          }

          type A implements I1 {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            i1 {
              ... on I2 {
                v
              }
            }
          }
        "#,


      // While `A` is a `I1` in the supergraph while not in `Subgraph1`, since the `i1`
      // operation is resolved by `Subgraph1`, it cannot ever return a A, and so
      // there is need to type-explode `I2` in this query (even if `Subgraph1` would
      // otherwise not include `A` from a `... on I2`).
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              i1 {
                __typename
                ... on I2 {
                  __typename
                  v
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn union_union_interaction() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            u1: U1
          }

          union U1 = A | B | C
          union U2 = B | C

          type A {
            v: Int @shareable
          }

          type B {
            v: Int
          }

          type C {
            v: Int
          }
        "#,
        Subgraph2: r#"
          union U2 = A

          type A {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u1 {
              ... on U2 {
                ... on A {
                  v
                }
              }
            }
          }
        "#,


      // Type `A` can be returned by `u1` and is a `U2` *in the supergraph* but not in `Subgraph1`, so need to
      // type-explode `U2` in the query to `Subgraph1` so it doesn't exclude `A`.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              u1 {
                __typename
                ... on A {
                  v
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
#[should_panic(
    expected = r#"Cannot add fragment of condition "A" (runtimes: [A]) to parent type "U1" (runtimes: [B, C])"#
)]
// TODO: investigate this failure
fn union_union_interaction_but_no_need_to_type_explode() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            u1: U1
          }

          union U1 = B | C
          union U2 = A | B | C

          type A {
            v: Int @shareable
          }

          type B {
            v: Int
          }

          type C {
            v: Int
          }
        "#,
        Subgraph2: r#"
          union U1 = A

          type A {
            v: Int @shareable
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            u1 {
              ... on U2 {
                ... on A {
                  v
                }
              }
            }
          }
        "#,


      // Similar case than in the `interface/union` case: the whole `... on U2` sub-selection happens to be
      // unsatisfiable in practice.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              u1 {
                __typename
              }
            }
          },
        }
      "###
    );
}

#[test]
fn handles_spread_unions_correctly() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          u: U
        }

        union U = A | B

        type A @key(fields: "id") {
          id: ID!
          a1: Int
        }

        type B {
          id: ID!
          b: Int
        }

        type C @key(fields: "id") {
          id: ID!
          c1: Int
        }
        "#,
        Subgraph2: r#"
        type Query {
          otherQuery: U
        }

        union U = A | C

        type A @key(fields: "id") {
          id: ID!
          a2: Int
        }

        type C @key(fields: "id") {
          id: ID!
          c2: Int
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          u {
            ... on C {
              c1
            }
          }
        }
      "#,


    // Note: it's important that the query below DO NOT include the `... on C` part. Because in
    // Subgraph 1, `C` is not a part of the union `U` and so a spread for `C` inside `u` is invalid
    // GraphQL.
        @r###"
      QueryPlan {
        Fetch(service: "Subgraph1") {
          {
            u {
              __typename
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
fn handles_case_of_key_chains_in_parallel_requires() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T
        }

        union T = T1 | T2

        type T1 @key(fields: "id1") {
          id1: ID!
        }

        type T2 @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
        Subgraph2: r#"
        type T1 @key(fields: "id1") @key(fields: "id2") {
          id1: ID!
          id2: ID!
        }
        "#,
        Subgraph3: r#"
        type T1 @key(fields: "id2") {
          id2: ID!
          x: Int
        }

        type T2 @key(fields: "id") {
          id: ID!
          y: Int @external
          z: Int @requires(fields: "y")
        }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
        {
          t {
            ... on T1 {
              x
            }
            ... on T2 {
              z
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
                ... on T1 {
                  __typename
                  id1
                }
                ... on T2 {
                  __typename
                  id
                  y
                }
              }
            }
          },
          Parallel {
            Sequence {
              Flatten(path: "t") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on T1 {
                      __typename
                      id1
                    }
                  } =>
                  {
                    ... on T1 {
                      id2
                    }
                  }
                },
              },
              Flatten(path: "t") {
                Fetch(service: "Subgraph3") {
                  {
                    ... on T1 {
                      __typename
                      id2
                    }
                  } =>
                  {
                    ... on T1 {
                      x
                    }
                  }
                },
              },
            },
            Flatten(path: "t") {
              Fetch(service: "Subgraph3") {
                {
                  ... on T2 {
                    __typename
                    id
                    y
                  }
                } =>
                {
                  ... on T2 {
                    z
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
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn handles_types_with_no_common_supertype_at_the_same_merge_at() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T
        }

        union T = T1 | T2

        type T1 @key(fields: "id") {
          id: ID!
          sub: Foo
        }

        type Foo @key(fields: "id") {
          id: ID!
          x: Int
        }

        type T2 @key(fields: "id") {
          id: ID!
          sub: Bar
        }

        type Bar @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
        Subgraph2: r#"
        type Foo @key(fields: "id") {
          id: ID!
          y: Int
        }

        type Bar @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          t {
            ... on T1 {
              sub {
                y
              }
            }
            ... on T2 {
              sub {
                y
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
              t {
                __typename
                ... on T1 {
                  sub {
                    __typename
                    id
                  }
                }
                ... on T2 {
                  sub {
                    __typename
                    id
                  }
                }
              }
            }
          },
          Flatten(path: "t.sub") {
            Fetch(service: "Subgraph2") {
              {
                ... on Foo {
                  __typename
                  id
                }
                ... on Bar {
                  __typename
                  id
                }
              } =>
              {
                ... on Foo {
                  y
                }
                ... on Bar {
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
fn does_not_error_out_handling_fragments_when_interface_subtyping_is_involved() {
    // This test essentially make sure the issue in https://github.com/apollographql/federation/issues/2592
    // is resolved.
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          a: A!
        }

        interface IA {
          b: IB!
        }

        type A implements IA {
          b: B!
        }

        interface IB {
          v1: Int!
        }

        type B implements IB {
          v1: Int!
          v2: Int!
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          a {
            ...F1
            ...F2
            ...F3
          }
        }

        fragment F1 on A {
          b {
            v2
          }
        }

        fragment F2 on IA {
          b {
            v1
          }
        }

        fragment F3 on IA {
          b {
            __typename
          }
        }
      "#,
        @r###"
      QueryPlan {
        Fetch(service: "Subgraph1") {
          {
            a {
              b {
                __typename
                v2
                v1
              }
            }
          }
        },
      }
    "###
    );
}
