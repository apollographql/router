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
                        ... on T {
                          __typename
                          a
                        }
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
        @r###"
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
                            ... on B {
                              __typename
                              idB
                              ... on B {
                                ... on B {
                                  __typename
                                  required
                                }
                              }
                            }
                          }
                        } =>
                        {
                          ... on B {
                            ... on B {
                              c
                            }
                          }
                        }
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
        @r###"
        QueryPlan {
          Include(if: $test) {
            Fetch(service: "Subgraph1") {
              {
                foo {
                  ... on Foo {
                    id
                  }
                }
              }
            },
          },
        }
        "###
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
        @r###"
        QueryPlan {
          Include(if: $test) {
            Fetch(service: "Subgraph1") {
              {
                foo {
                  ... on Foo {
                    id
                    bar {
                      ... on Bar {
                        id
                      }
                    }
                  }
                }
              }
            },
          },
        }
        "###
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
        @r###"
        QueryPlan {
          Include(if: $test) {
            Fetch(service: "Subgraph1") {
              {
                foo {
                  id
                  foo {
                    ... on Foo {
                      bar {
                        id
                      }
                    }
                  }
                }
              }
            },
          },
        }
        "###
    );
}

/// Two sibling fragment spreads carry opposite conditions on the *same* variable, so neither
/// condition can be hoisted into an `Include`/`Skip` plan node — they have to survive into the
/// subgraph operation. Both spreads select a field whose `@requires` is satisfied by another
/// subgraph, which forces a post-`@requires` fetch back into Subgraph1.
///
/// That fetch used to lose both conditions: its path is rebuilt from `OpGraphPathContext`, which
/// only accumulates conditionals across subgraph-jump edges, so the two branches produced the same
/// unconditional path and merged into one node. The subgraph was then asked for `groups` and
/// `listItems` unconditionally, with the union of both `@requires` sets as inputs — demanding
/// `offerId`, which is only fetched on the `@skip` branch.
///
/// The two `@requires` sets differ (`listItems` also needs `offerId`) so that the merge is
/// observable; with identical sets the conditions were dropped just the same, only silently.
#[test]
fn conditions_survive_a_post_requires_fetch() {
    let planner = planner!(
        Subgraph1: r#"
            type Query {
              shoppingListDetails: ShoppingListDetails
            }

            type ShoppingListDetails {
              items: [ProductList!]
            }

            type ProductList @key(fields: "productsInfo") {
              productsInfo: String
              products: [Product] @external
              groups: [Group!] @requires(fields: "products { usItemId }")
              listItems: [ListItem!] @requires(fields: "products { usItemId offerId }")
            }

            type Product @key(fields: "usItemId", resolvable: false) {
              usItemId: ID!
              offerId: ID @external
            }

            type Group {
              groupId: ID!
            }

            type ListItem {
              listItemId: ID!
            }
        "#,
        Subgraph2: r#"
            type ProductList @key(fields: "productsInfo") {
              productsInfo: String
              products: [Product]
            }

            type Product @key(fields: "usItemId") {
              usItemId: ID!
              offerId: ID
            }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
            query foo($useGroupItems: Boolean!) {
              shoppingListDetails {
                items {
                  ...GroupItems @include(if: $useGroupItems)
                  ...FlatItems @skip(if: $useGroupItems)
                }
              }
            }

            fragment GroupItems on ProductList { groups { groupId } }
            fragment FlatItems on ProductList { listItems { listItemId } }
          "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph1") {
              {
                shoppingListDetails {
                  items {
                    ... on ProductList @include(if: $useGroupItems) {
                      __typename
                      productsInfo
                    }
                    ... on ProductList @skip(if: $useGroupItems) {
                      __typename
                      productsInfo
                    }
                  }
                }
              }
            },
            Flatten(path: "shoppingListDetails.items.@") {
              Fetch(service: "Subgraph2") {
                {
                  ... on ProductList {
                    __typename
                    productsInfo
                  }
                  ... on ProductList {
                    __typename
                    productsInfo
                  }
                } =>
                {
                  ... on ProductList @include(if: $useGroupItems) {
                    products {
                      usItemId
                    }
                  }
                  ... on ProductList @skip(if: $useGroupItems) {
                    products {
                      usItemId
                      offerId
                    }
                  }
                }
              },
            },
            Flatten(path: "shoppingListDetails.items.@") {
              Fetch(service: "Subgraph1") {
                {
                  ... on ProductList {
                    __typename
                    products {
                      usItemId
                    }
                    productsInfo
                  }
                  ... on ProductList {
                    __typename
                    products {
                      usItemId
                      offerId
                    }
                    productsInfo
                  }
                } =>
                {
                  ... on ProductList @include(if: $useGroupItems) {
                    groups {
                      groupId
                    }
                  }
                  ... on ProductList @skip(if: $useGroupItems) {
                    listItems {
                      listItemId
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
