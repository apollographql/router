use apollo_federation::query_plan::FetchDataRewrite;

use crate::query_plan::build_query_plan_support::find_fetch_nodes_for_subgraph;

#[test]
fn it_handles_a_simple_at_requires_triggered_within_a_conditional() {
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
            query foo($test: Boolean!) {
              t @include(if: $test) {
                b
              }
            }
          "#,
        @r###"
          QueryPlan {
            Include(if: $test) {
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
            },
          }
        "###
    );
}

#[test]
fn it_handles_an_at_requires_triggered_conditionally() {
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
            query foo($test: Boolean!) {
              t {
                b @include(if: $test)
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
                    ... on T @include(if: $test) {
                      a
                    }
                  }
                }
              },
              Include(if: $test) {
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
            },
          }
        "###
    );
}

#[test]
fn it_handles_an_at_requires_where_multiple_conditional_are_involved() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              a: A
            }
  
            type A @key(fields: "idA") {
              idA: ID!
            }
        "#,
        Subgraph2: r#"
            type A @key(fields: "idA") {
              idA: ID!
              b: [B]
            }
  
            type B @key(fields: "idB") {
              idB: ID!
              required: Int
            }
        "#,
        Subgraph3: r#"
            type B @key(fields: "idB") {
              idB: ID!
              c: Int @requires(fields: "required")
              required: Int @external
            }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
            query foo($test1: Boolean!, $test2: Boolean!) {
              a @include(if: $test1) {
                b @include(if: $test2) {
                  c
                }
              }
            }
          "#,
        @r#"
    QueryPlan {
      Include(if: $test1) {
        Sequence {
          Fetch(service: "Subgraph1") {
            {
              a {
                __typename
                idA
              }
            }
          },
          Include(if: $test2) {
            Sequence {
              Flatten(path: "a") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on A {
                      __typename
                      idA
                    }
                  } =>
                  {
                    ... on A {
                      b {
                        __typename
                        idB
                        required
                      }
                    }
                  }
                },
              },
              Flatten(path: "a.b.@") {
                Fetch(service: "Subgraph3") {
                  {
                    ... on B {
                      __typename
                      idB
                      required
                    }
                  } =>
                  {
                    ... on B {
                      c
                    }
                  }
                },
              },
            },
          },
        },
      },
    }
    "#
    );
}

#[test]
fn unnecessary_include_is_stripped_from_fragments() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              foo: Foo,
            }
            type Foo @key(fields: "id") {
              id: ID,
              bar: Bar,
            }
            type Bar @key(fields: "id") {
              id: ID,
            }
        "#,
        Subgraph2: r#"
            type Bar @key(fields: "id") {
              id: ID,
              a: Int,
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        query foo($test: Boolean!) {
          foo @include(if: $test) {
            ... on Foo @include(if: $test) {
              id
            }
          }
        }
        "#,
        @r#"
    QueryPlan {
      Include(if: $test) {
        Fetch(service: "Subgraph1") {
          {
            foo {
              id
            }
          }
        },
      },
    }
    "#
    );
    assert_plan!(
        &planner,
        r#"
        query foo($test: Boolean!) {
          foo @include(if: $test) {
            ... on Foo @include(if: $test) {
              id
              bar {
                ... on Bar @include(if: $test) {
                  id
                }
              }
            }
          }
        }
        "#,
        @r#"
    QueryPlan {
      Include(if: $test) {
        Fetch(service: "Subgraph1") {
          {
            foo {
              id
              bar {
                id
              }
            }
          }
        },
      },
    }
    "#
    );
}

#[test]
fn selections_are_not_overwritten_after_removing_directives() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              foo: Foo,
            }
            type Foo @key(fields: "id") {
              id: ID,
              foo: Foo,
              bar: Bar,
            }
            type Bar @key(fields: "id") {
              id: ID,
            }
        "#,
        Subgraph2: r#"
            type Bar @key(fields: "id") {
              id: ID,
              a: Int,
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query foo($test: Boolean!) {
            foo @include(if: $test) {
              ... on Foo {
                id
                foo {
                  ... on Foo @include(if: $test) {
                    bar {
                      id
                    }
                  }
                }
              }
            }
          }
          "#,
        @r#"
    QueryPlan {
      Include(if: $test) {
        Fetch(service: "Subgraph1") {
          {
            foo {
              id
              foo {
                bar {
                  id
                }
              }
            }
          }
        },
      },
    }
    "#
    );
}

#[test]
fn it_aliases_multiple_requires_under_a_conditional_root_fragment() {
    // Same subgraphs as `it_handles_multiple_requires_within_the_same_entity_fetch`. Once the
    // `@include` is turned into a condition node, the root fragment is left without a type
    // condition. The conflicting `f` selections inside it must still be aliased, or the
    // Subgraph1 fetch is invalid GraphQL.
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
    let plan = assert_plan!(
        &planner,
        r#"
          query ($a: Boolean!) {
            ... on Query @include(if: $a) {
              is {
                g
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Include(if: $a) {
            Sequence {
              Fetch(service: "Subgraph1") {
                {
                  ... {
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
          },
        }
      "###
    );
    // The aliased `f` must be renamed back before it's used as a `@requires` input.
    let fetch_nodes = find_fetch_nodes_for_subgraph("Subgraph1", &plan);
    assert_eq!(fetch_nodes.len(), 1);
    let renames = fetch_nodes[0]
        .output_rewrites
        .iter()
        .map(|rewrite| match rewrite.as_ref() {
            FetchDataRewrite::KeyRenamer(renamer) => format!(
                "{} -> {}",
                renamer
                    .path
                    .iter()
                    .map(|element| element.to_string())
                    .collect::<Vec<_>>()
                    .join("/"),
                renamer.rename_key_to
            ),
            other => panic!("unexpected rewrite {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(renames, ["is/... on T3/f__alias_0 -> f"]);
}

/// A field with `@requires` is under `@include`, and shares its subgraph (and fetch) with an
/// unconditional sibling. The required field is fetched in the parent fetch only under the
/// condition, so the inputs of the entity fetch must carry that condition too: when the condition
/// is false, the required field is absent from the parent's response and must not be part of the
/// representation, or the unconditional sibling would never be fetched (FED-707).
#[test]
fn conditional_requires_sharing_a_fetch_with_an_unconditional_sibling() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              a: Int!
            }
        "#,
        Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              a: Int! @external
              b: Int! @requires(fields: "a")
              c: Int!
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query foo($test: Boolean!) {
              t {
                c
                b @include(if: $test)
              }
            }
          "#,
        @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              __typename
              id
              ... on T @include(if: $test) {
                a
              }
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
              ... on T @include(if: $test) {
                __typename
                a
              }
            } =>
            {
              ... on T {
                c
                b @include(if: $test)
              }
            }
          },
        },
      },
    }
    "#
    );
}

/// Same as above, but with a literal condition: the planner keeps the statically-excluded
/// selections, so the inputs must be conditional here too for the sibling to be fetched.
#[test]
fn conditional_requires_sharing_a_fetch_with_an_unconditional_sibling_literal_condition() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              a: Int!
            }
        "#,
        Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              a: Int! @external
              b: Int! @requires(fields: "a")
              c: Int!
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query foo {
              t {
                c
                b @include(if: false)
              }
            }
          "#,
        @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              __typename
              id
              ... on T @include(if: false) {
                a
              }
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
              ... on T @include(if: false) {
                __typename
                a
              }
            } =>
            {
              ... on T {
                c
                b @include(if: false)
              }
            }
          },
        },
      },
    }
    "#
    );
}

/// The required field set is an object whose fields are non-nullable, but the object itself is
/// nullable. Without conditional inputs, the representation would be sent with the object set to
/// `null` instead of being left out, which subgraphs may reject.
#[test]
fn conditional_requires_of_a_nullable_object_with_non_nullable_fields() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              t: T
            }

            type T @key(fields: "id") {
              id: ID!
              u: U
            }

            type U {
              x: Int!
              y: Int!
            }
        "#,
        Subgraph2: r#"
            type T @key(fields: "id") {
              id: ID!
              u: U @external
              b: Int! @requires(fields: "u { x }")
              c: Int!
            }

            type U {
              x: Int! @external
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query foo($test: Boolean!) {
              t {
                c
                b @include(if: $test)
              }
            }
          "#,
        @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              __typename
              id
              ... on T @include(if: $test) {
                u {
                  x
                }
              }
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
              ... on T @include(if: $test) {
                __typename
                u {
                  x
                }
              }
            } =>
            {
              ... on T {
                c
                b @include(if: $test)
              }
            }
          },
        },
      },
    }
    "#
    );
}

/// Two sibling fragment spreads carry opposite conditions on the *same* variable, so neither
/// condition can be hoisted into an `Include`/`Skip` plan node. Both spreads select a field whose
/// `@requires` is satisfied by another subgraph, forcing a post-`@requires` fetch back into
/// Subgraph1. The conditions live in the fetch node path rather than in the path context, and
/// used to be lost by that fetch: the subgraph was asked for both `grouped` and `flat`
/// unconditionally, with the union of both `@requires` sets as inputs (TSH-24808).
#[test]
fn opposite_conditions_on_fields_with_different_requires() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              collections: [Collection!]
            }

            type Collection @key(fields: "info") {
              info: String
              items: [Item] @external
              grouped: [Grouped!] @requires(fields: "items { itemId }")
              flat: [Flat!] @requires(fields: "items { itemId extraId }")
            }

            type Item @key(fields: "itemId", resolvable: false) {
              itemId: ID!
              extraId: ID @external
            }

            type Grouped {
              groupedKey: ID!
            }

            type Flat {
              flatKey: ID!
            }
        "#,
        Subgraph2: r#"
            type Collection @key(fields: "info") {
              info: String
              items: [Item]
            }

            type Item @key(fields: "itemId") {
              itemId: ID!
              extraId: ID
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query Q($useGrouped: Boolean!) {
              collections {
                ...GroupedView @include(if: $useGrouped)
                ...FlatView @skip(if: $useGrouped)
              }
            }

            fragment GroupedView on Collection {
              grouped {
                groupedKey
              }
            }

            fragment FlatView on Collection {
              flat {
                flatKey
              }
            }
          "#,
        @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            collections {
              ... on Collection @include(if: $useGrouped) {
                __typename
                info
              }
              ... on Collection @skip(if: $useGrouped) {
                __typename
                info
              }
            }
          }
        },
        Flatten(path: "collections.@") {
          Fetch(service: "Subgraph2") {
            {
              ... on Collection @include(if: $useGrouped) {
                __typename
                info
              }
              ... on Collection @skip(if: $useGrouped) {
                __typename
                info
              }
            } =>
            {
              ... on Collection @include(if: $useGrouped) {
                items {
                  itemId
                }
              }
              ... on Collection @skip(if: $useGrouped) {
                items {
                  itemId
                  extraId
                }
              }
            }
          },
        },
        Flatten(path: "collections.@") {
          Fetch(service: "Subgraph1") {
            {
              ... on Collection @include(if: $useGrouped) {
                __typename
                items {
                  itemId
                }
                info
              }
              ... on Collection @skip(if: $useGrouped) {
                __typename
                items {
                  itemId
                  extraId
                }
                info
              }
            } =>
            {
              ... on Collection @include(if: $useGrouped) {
                grouped {
                  groupedKey
                }
              }
              ... on Collection @skip(if: $useGrouped) {
                flat {
                  flatKey
                }
              }
            }
          },
        },
      },
    }
    "#
    );
}

/// When every path into a `@requires` field is conditional, the post-requires fetch itself
/// gets a condition node instead of being executed unconditionally.
#[test]
fn requires_under_a_fully_conditional_fragment_gets_a_condition_node() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              t: [T]
            }

            type T @key(fields: "id") {
              id: ID!
              inner: Inner
              derived: Derived @requires(fields: "inner { u { x } }")
            }

            type Inner {
              u: U
            }

            type U @key(fields: "id") {
              id: ID!
              x: ID @external
            }

            type Derived {
              y: String
            }
        "#,
        Subgraph2: r#"
            type U @key(fields: "id") {
              id: ID!
              x: ID
              z: Int
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query Q($s: Boolean = false) {
              t {
                ... on T @skip(if: $s) {
                  derived {
                    y
                  }
                }
                ... on T @include(if: $s) {
                  inner {
                    u {
                      z
                    }
                  }
                }
              }
            }
          "#,
        @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "Subgraph1") {
          {
            t {
              ... on T @skip(if: $s) {
                __typename
                inner {
                  u {
                    __typename
                    id
                  }
                }
                id
              }
              ... on T @include(if: $s) {
                inner {
                  u {
                    __typename
                    id
                  }
                }
              }
            }
          }
        },
        Flatten(path: "t.@.inner.u") {
          Fetch(service: "Subgraph2") {
            {
              ... on U @skip(if: $s) {
                __typename
                id
              }
              ... on U @include(if: $s) {
                __typename
                id
              }
            } =>
            {
              ... on U @skip(if: $s) {
                x
              }
              ... on U @include(if: $s) {
                z
              }
            }
          },
        },
        Skip(if: $s) {
          Flatten(path: "t.@") {
            Fetch(service: "Subgraph1") {
              {
                ... on T {
                  __typename
                  inner {
                    u {
                      x
                    }
                  }
                  id
                }
              } =>
              {
                ... on T {
                  derived {
                    y
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

#[test]
fn conditional_requiring_field_on_an_interface_member_is_not_double_wrapped() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              cards: [Card]
            }

            interface Card {
              id: ID!
            }

            type GridCard implements Card @key(fields: "id") {
              id: ID!
              odds: Int @external
              display: String @requires(fields: "odds")
            }

            type ListCard implements Card @key(fields: "id") {
              id: ID!
            }
        "#,
        Subgraph2: r#"
            type GridCard @key(fields: "id") {
              id: ID!
              odds: Int
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
            query Q($a: Boolean!, $v: Boolean!) {
              cards @include(if: $a) {
                id
                ... on GridCard {
                  display @include(if: $v)
                }
              }
            }
          "#,
        @r#"
    QueryPlan {
      Include(if: $a) {
        Sequence {
          Fetch(service: "Subgraph1") {
            {
              cards {
                __typename
                id
                ... on GridCard {
                  __typename
                  id
                }
              }
            }
          },
          Include(if: $v) {
            Sequence {
              Flatten(path: "cards.@") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on GridCard {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on GridCard {
                      odds
                    }
                  }
                },
              },
              Flatten(path: "cards.@") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on GridCard {
                      __typename
                      odds
                      id
                    }
                  } =>
                  {
                    ... on GridCard {
                      display
                    }
                  }
                },
              },
            },
          },
        },
      },
    }
    "#
    );
}
