/*
Template to copy-paste:

#[test]
fn some_name() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            ...
          }
        "#,
        Subgraph2: r#"
          type Query {
            ...
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            ...
          }
        "#,
        @r###"
          QueryPlan {
            ...
          }
        "###
    );
}
*/

mod debug_max_evaluated_plans_configuration;
mod fetch_operation_names;
mod field_merging_with_skip_and_include;
mod fragment_autogeneration;
mod handles_fragments_with_directive_conditions;
mod handles_operations_with_directives;
mod interface_object;
mod interface_type_explosion;
mod introspection_typename_handling;
mod merged_abstract_types_handling;
mod mutations;
mod named_fragments;
mod named_fragments_preservation;
mod overrides;
mod provides;
mod requires;
mod shareable_root_fields;
mod subscriptions;
// TODO: port the rest of query-planner-js/src/__tests__/buildPlan.test.ts

#[test]
fn pick_keys_that_minimize_fetches() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            transfers: [Transfer!]!
          }

          type Transfer @key(fields: "from { iso } to { iso }") {
            from: Country!
            to: Country!
          }

          type Country @key(fields: "iso") {
            iso: String!
          }
        "#,
        Subgraph2: r#"
          type Transfer @key(fields: "from { iso } to { iso }") {
            id: ID!
            from: Country!
            to: Country!
          }

          type Country @key(fields: "iso") {
            iso: String!
            currency: Currency!
          }

          type Currency {
            name: String!
            sign: String!
          }
        "#,
    );
    // We want to make sure we use the key on Transfer just once,
    // not 2 fetches using the keys on Country.
    assert_plan!(
        &planner,
        r#"
          {
            transfers {
              from {
                currency {
                  name
                }
              }
              to {
                currency {
                  sign
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
                  transfers {
                    __typename
                    from {
                      iso
                    }
                    to {
                      iso
                    }
                  }
                }
              },
              Flatten(path: "transfers.@") {
                Fetch(service: "Subgraph2") {
                  {
                    ... on Transfer {
                      __typename
                      from {
                        iso
                      }
                      to {
                        iso
                      }
                    }
                  } =>
                  {
                    ... on Transfer {
                      from {
                        currency {
                          name
                        }
                      }
                      to {
                        currency {
                          sign
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
}

/// This tests the issue from https://github.com/apollographql/federation/issues/1858.
/// That issue, which was a bug in the handling of selection sets, was concretely triggered with
/// a mix of an interface field implemented with some covariance and the query plan using
/// type-explosion.
/// That error can be reproduced on a pure fed2 example, it's just a bit more
/// complex as we need to involve a @provide just to force the query planner to type explode
/// (more precisely, this force the query planner to _consider_ type explosion; the generated
/// query plan still ends up not type-exploding in practice since as it's not necessary).
#[test]
fn field_covariance_and_type_explosion() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          dummy: Interface
        }

        interface Interface {
          field: Interface
        }

        type Object implements Interface @key(fields: "id") {
          id: ID!
          field: Object @provides(fields: "x")
          x: Int @external
        }
        "#,
        Subgraph2: r#"
        type Object @key(fields: "id") {
          id: ID!
          x: Int @shareable
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          dummy {
            field {
              ... on Object {
                field {
                  __typename
                }
              }
            }
          }
        }
        "#,
        @r###"
    QueryPlan {
      Fetch(service: "Subgraph1") {
        {
          dummy {
            __typename
            field {
              __typename
              ... on Object {
                field {
                  __typename
                }
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
fn handles_non_intersecting_fragment_conditions() {
    let planner = planner!(
        Subgraph1: r#"
            interface Fruit {
              edible: Boolean!
            }

            type Banana implements Fruit {
              edible: Boolean!
              inBunch: Boolean!
            }

            type Apple implements Fruit {
              edible: Boolean!
              hasStem: Boolean!
            }

            type Query {
              fruit: Fruit!
            }
          "#,
    );
    assert_plan!(
        &planner,
        r#"
            fragment OrangeYouGladIDidntSayBanana on Fruit {
              ... on Banana {
                inBunch
              }
              ... on Apple {
                hasStem
              }
            }

            query Fruitiness {
              fruit {
                ... on Apple {
                  ...OrangeYouGladIDidntSayBanana
                }
              }
            }
          "#,
          @r#"
          QueryPlan {
            Fetch(service: "Subgraph1") {
              {
                fruit {
                  __typename
                  ... on Apple {
                    hasStem
                  }
                }
              }
            },
          }
          "#
    );
}

#[test]
fn avoids_unnecessary_fetches() {
    // This test is a reduced example demonstrating a previous issue with the computation of query plans cost.
    // The general idea is that "Subgraph 3" has a declaration that is kind of useless (it declares entity A
    // that only provides it's own key, so there is never a good reason to use it), but the query planner
    // doesn't know that and will "test" plans including fetch to that subgraphs in its exhaustive search
    // of all options. In theory, the query plan costing mechanism should eliminate such plans in favor of
    // plans not having this inefficient, but an issue in the plan cost computation led to such inefficient
    // to have the same cost as the more efficient one and to be picked (just because it was the first computed).
    // This test ensures this costing bug is fixed.

    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "idT") {
            idT: ID!
            a: A
          }

          type A @key(fields: "idA2") {
            idA2: ID!
          }
          "#,
        Subgraph2: r#"
          type T @key(fields: "idT") {
            idT: ID!
            u: U
          }

          type U @key(fields: "idU") {
            idU: ID!
          }
          "#,
        Subgraph3: r#"
          type A @key(fields: "idA1") {
            idA1: ID!
          }
          "#,
        Subgraph4: r#"
          type A @key(fields: "idA1") @key(fields: "idA2") {
            idA1: ID!
            idA2: ID!
          }
          "#,
        Subgraph5: r#"
          type U @key(fields: "idU") {
            idU: ID!
            v: Int
          }
          "#,
    );

    assert_plan!(
        &planner,
        r#"
          {
            t {
              u {
                v
              }
              a {
                idA1
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
                  __typename
                  idT
                  a {
                    __typename
                    idA2
                  }
                }
              }
            },
            Parallel {
              Flatten(path: "t.a") {
                Fetch(service: "Subgraph4") {
                  {
                    ... on A {
                      __typename
                      idA2
                    }
                  } =>
                  {
                    ... on A {
                      idA1
                    }
                  }
                },
              },
              Sequence {
                Flatten(path: "t") {
                  Fetch(service: "Subgraph2") {
                    {
                      ... on T {
                        __typename
                        idT
                      }
                    } =>
                    {
                      ... on T {
                        u {
                          __typename
                          idU
                        }
                      }
                    }
                  },
                },
                Flatten(path: "t.u") {
                  Fetch(service: "Subgraph5") {
                    {
                      ... on U {
                        __typename
                        idU
                      }
                    } =>
                    {
                      ... on U {
                        v
                      }
                    }
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
fn it_executes_mutation_operations_in_sequence() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            q1: Int
          }

          type Mutation {
            m1: Int
          }
        "#,
        Subgraph2: r#"
          type Mutation {
            m2: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          mutation {
            m2
            m1
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                m2
              }
            },
            Fetch(service: "Subgraph1") {
              {
                m1
              }
            },
          },
        }
      "###
    );
}

/// @requires references external field indirectly
#[test]
fn key_where_at_external_is_not_at_top_level_of_selection_of_requires() {
    // Field issue where we were seeing a FetchGroup created where the fields used by the key to jump subgraphs
    // were not properly fetched. In the below test, this test will ensure that 'k2' is properly collected
    // before it is used
    let planner = planner!(
        A: r#"
          type Query {
            u: U!
          }

          type U @key(fields: "k1 { id }") {
            k1: K
          }

          type K @key(fields: "id") {
            id: ID!
          }
        "#,
        B: r#"
          type U @key(fields: "k1 { id }") @key(fields: "k2") {
            k1: K!
            k2: ID!
            v: V! @external
            f: ID! @requires(fields: "v { v }")
            f2: Int!
          }

          type K @key(fields: "id") {
            id: ID!
          }

          type V @key(fields: "id") {
            id: ID!
            v: String! @external
          }
        "#,
        C: r#"
          type U @key(fields: "k1 { id }") @key(fields: "k2") {
            k1: K!
            k2: ID!
            v: V!
          }

          type K @key(fields: "id") {
            id: ID!
          }

          type V @key(fields: "id") {
            id: ID!
            v: String!
          }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
          {
            u {
              f
            }
          }
        "#,
        @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "A") {
              {
                u {
                  __typename
                  k1 {
                    id
                  }
                }
              }
            },
            Flatten(path: "u") {
              Fetch(service: "B") {
                {
                  ... on U {
                    __typename
                    k1 {
                      id
                    }
                  }
                } =>
                {
                  ... on U {
                    k2
                  }
                }
              },
            },
            Flatten(path: "u") {
              Fetch(service: "C") {
                {
                  ... on U {
                    __typename
                    k2
                  }
                } =>
                {
                  ... on U {
                    v {
                      v
                    }
                  }
                }
              },
            },
            Flatten(path: "u") {
              Fetch(service: "B") {
                {
                  ... on U {
                    __typename
                    v {
                      v
                    }
                    k1 {
                      id
                    }
                  }
                } =>
                {
                  ... on U {
                    f
                  }
                }
              },
            },
          },
        }
      "###
    );
}

// TODO(@TylerBloom): As part of the private preview, we strip out all uses of the @defer
// directive. Once handling that feature is implemented, this test will start failing and should be
// updated to use a config for the planner to strip out the defer directive.
#[test]
fn defer_gets_stripped_out() {
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
            data: String
          }
          "#,
    );
    let plan_one = assert_plan!(
        &planner,
        r#"
          {
              t {
                  id
                  data
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
                      data
                    }
                  }
                },
              },
            },
          }
        "###
    );
    let plan_two = assert_plan!(
        &planner,
        r#"
          {
              t {
                  id
                  ... @defer {
                    data
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
                      data
                    }
                  }
                },
              },
            },
          }
        "###
    );
    assert_eq!(plan_one, plan_two)
}

#[test]
fn test_merging_fetches_do_not_create_cycle_in_fetch_dependency_graph() {
    // This is a test for ROUTER-546 (the second part).
    let planner = planner!(
        S: r#"
          type Query {
            start: T!
          }

          type T @key(fields: "id") {
            id: String!
          }
          "#,
        A: r#"
          type T @key(fields: "id") {
            id: String! @shareable
            u: U! @shareable
          }

          type U @key(fields: "id") {
            id: ID!
            a: String! @shareable
            b: String @shareable
          }
          "#,
        B: r#"
          type T @key(fields: "id") {
            id: String! @external
            u: U! @shareable
          }

          type U @key(fields: "id") {
            id: ID!
            a: String! @shareable
            # Note: b is not here.
          }

          # This definition is necessary.
          extend type W @key(fields: "id") {
            id: ID @external
          }
          "#,
        C: r#"
          extend type U @key(fields: "id") {
            id: ID! @external
            a: String! @external
            b: String @external
            w: W @requires(fields: "a b")
          }

          type W @key(fields: "id") {
            id: ID
            y: Y
            w1: Int
            w2: Int
            w3: Int
            w4: Int
            w5: Int
          }

          type Y {
            y1: Int
            y2: Int
            y3: Int
          }
          "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            start {
              u {
                w {
                  id
                  w1
                  w2
                  w3
                  w4
                  w5
                  y {
                    y1
                    y2
                    y3
                  }
                }
              }
            }
          }
        "#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "S") {
          {
            start {
              __typename
              id
            }
          }
        },
        Parallel {
          Sequence {
            Flatten(path: "start") {
              Fetch(service: "B") {
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
                    }
                  }
                }
              },
            },
            Flatten(path: "start.u") {
              Fetch(service: "A") {
                {
                  ... on U {
                    __typename
                    id
                  }
                } =>
                {
                  ... on U {
                    b
                    a
                  }
                }
              },
            },
          },
          Flatten(path: "start") {
            Fetch(service: "A") {
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
                    b
                    a
                  }
                }
              }
            },
          },
        },
        Flatten(path: "start.u") {
          Fetch(service: "C") {
            {
              ... on U {
                __typename
                a
                b
                id
              }
            } =>
            {
              ... on U {
                w {
                  y {
                    y1
                    y2
                    y3
                  }
                  id
                  w1
                  w2
                  w3
                  w4
                  w5
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
fn redundant_typename_for_inline_fragments_without_type_condition() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            products: [Product]
          }
          interface Product {
            name: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            products {
              ... @skip(if: false) {
                name
              }
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              products {
                __typename
                ... @skip(if: false) {
                  name
                }
              }
            }
          },
        }
        "###
    );
}

#[test]
fn test_merging_fetches_reset_cached_costs() {
    // This is a test for ROUTER-553.
    let planner = planner!(
      A: r#"
            type Query {
                start: S @shareable
            }

            type S @key(fields: "id") {
                id: ID!
                u: U @shareable
            }

            type U @key(fields: "id") {
                id: ID!
            }
        "#,
      B: r#"
            type Query {
                start: S @shareable
            }

            type S @key(fields: "id") {
                id: ID!
            }
        "#,
      C: r#"
            type S @key(fields: "id") {
                id: ID!
                x: X
                a: String!
            }

            type X {
                t: T
            }

            type T {
                u: U @shareable
            }

            type U @key(fields: "id") {
                id: ID!
                b: String
            }
        "#,
    );
    assert_plan!(
        &planner,
        r#"{
            start {
                u {
                    b
                }
                a
                x {
                    t {
                        u {
                        id
                        }
                    }
                }
            }
        }"#,
        @r###"
    QueryPlan {
      Sequence {
        Fetch(service: "A") {
          {
            start {
              __typename
              u {
                __typename
                id
              }
              id
            }
          }
        },
        Parallel {
          Flatten(path: "start") {
            Fetch(service: "C") {
              {
                ... on S {
                  __typename
                  id
                }
              } =>
              {
                ... on S {
                  a
                  x {
                    t {
                      u {
                        id
                      }
                    }
                  }
                }
              }
            },
          },
          Flatten(path: "start.u") {
            Fetch(service: "C") {
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
    }
    "###
    );
}

#[test]
fn handles_multiple_conditions_on_abstract_types() {
    let planner = planner!(
        books: r#"
        type Book @key(fields: "id") {
          id: ID!
          title: String
        }
        "#,
        magazines: r#"
        type Magazine @key(fields: "id") {
          id: ID!
          title: String
        }
        "#,
        products: r#"
        type Query {
          products: [Product]
        }

        interface Product {
          id: ID!
          sku: String
          dimensions: ProductDimension
        }

        type ProductDimension @shareable {
          size: String
          weight: Float
        }

        type Book implements Product @key(fields: "id") {
          id: ID!
          sku: String
          dimensions: ProductDimension @shareable
        }

        type Magazine implements Product @key(fields: "id") {
          id: ID!
          sku: String
          dimensions: ProductDimension @shareable
        }
        "#,
        reviews: r#"
        type Book implements Product @key(fields: "id") {
          id: ID!
          reviews: [Review!]!
        }

        type Magazine implements Product @key(fields: "id") {
          id: ID!
          reviews: [Review!]!
        }

        interface Product {
          id: ID!
          reviews: [Review!]!
        }

        type Review {
          id: Int!
          body: String!
          product: Product
        }
        "#,
    );

    assert_plan!(
      &planner,
      r#"
      query ($title: Boolean = true) {
        products {
          id
          reviews {
            product {
              id
              ... on Book @include(if: $title) {
                title
                ... on Book @skip(if: $title) {
                  sku
                }
              }
              ... on Magazine {
                sku
              }
            }
          }
        }
      }
      "#,
      @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "products") {
              {
                products {
                  __typename
                  id
                  ... on Book {
                    __typename
                    id
                  }
                  ... on Magazine {
                    __typename
                    id
                  }
                }
              }
            },
            Flatten(path: "products.@") {
              Fetch(service: "reviews") {
                {
                  ... on Book {
                    __typename
                    id
                  }
                  ... on Magazine {
                    __typename
                    id
                  }
                } =>
                {
                  ... on Book {
                    reviews {
                      product {
                        __typename
                        id
                        ... on Book @include(if: $title) {
                          __typename
                          id
                          ... on Book @skip(if: $title) {
                            __typename
                            id
                          }
                        }
                        ... on Magazine {
                          __typename
                          id
                        }
                      }
                    }
                  }
                  ... on Magazine {
                    reviews {
                      product {
                        __typename
                        id
                        ... on Book @include(if: $title) {
                          __typename
                          id
                          ... on Book @skip(if: $title) {
                            __typename
                            id
                          }
                        }
                        ... on Magazine {
                          __typename
                          id
                        }
                      }
                    }
                  }
                }
              },
            },
            Parallel {
              Flatten(path: "products.@.reviews.@.product") {
                Fetch(service: "products") {
                  {
                    ... on Book {
                      ... on Book {
                        __typename
                        id
                      }
                    }
                    ... on Magazine {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Book @skip(if: $title) {
                      ... on Book @include(if: $title) {
                        sku
                      }
                    }
                    ... on Magazine {
                      sku
                    }
                  }
                },
              },
              Include(if: $title) {
                Flatten(path: "products.@.reviews.@.product") {
                  Fetch(service: "books") {
                    {
                      ... on Book {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on Book {
                        title
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
