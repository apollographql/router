// PORT_NOTE: The context tests in the JS code had more involved setup compared to the other tests.
// Here is a snippet from the JS context test leading up to the creation of the planner:
// ```js
//   const asFed2Service = (service: ServiceDefinition) => {
//     return {
//       ...service,
//       typeDefs: asFed2SubgraphDocument(service.typeDefs, {
//         includeAllImports: true,
//       }),
//     };
//   };
//
//   const composeAsFed2Subgraphs = (services: ServiceDefinition[]) => {
//     return composeServices(services.map((s) => asFed2Service(s)));
//   };
//
//   const result = composeAsFed2Subgraphs([subgraph1, subgraph2]);
//   const [api, queryPlanner] = [
//     result.schema!.toAPISchema(),
//     new QueryPlanner(Supergraph.buildForTests(result.supergraphSdl!)),
//   ];
// ```
// For all other tests, the set up was a single line:
// ```js
//  const [api, queryPlanner] = composeAndCreatePlanner(subgraph1, subgraph2);
// ```
//
// How this needs to be ported remains to be seen...

#[test]
fn set_context_test_variable_is_from_same_subgraph() {
    let planner = planner!(
      Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
      "#,
      Subgraph2: r#"
        type Query {
          a: Int!
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
            u {
              b
              field
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
                prop
                u {
                  __typename
                  id
                  b
                }
              }
            }
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: How should these be ported??
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on T', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above");
}

#[test]
fn set_context_test_variable_is_from_different_subgraph() {
    let planner = planner!(
    Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String! @external
        }
        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
      "#,
    Subgraph2: r#"
        type Query {
          a: Int!
        }
        type T @key(fields: "id") {
          id: ID!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
        }
      "#,
      );
    assert_plan!(
            planner,
            r#"
        {
          t {
            u {
              id
              field
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
                u {
                  __typename
                  id
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
              } =>
              {
                ... on T {
                  prop
                }
              }
            },
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
    "###);

    /* TODO: Port
    expect((plan as any).node.nodes[2].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on T', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above");
}

#[test]
fn set_context_test_variable_is_already_in_a_different_fetch_group() {
    let planner = planner!(
      Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
        }
      "#,
      Subgraph2: r#"
        type Query {
          a: Int!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          prop: String! @external
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
      "#,
    );

    assert_plan!(planner,
        r#"
        {
          t {
            u {
              id
              field
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
                prop
                u {
                  __typename
                  id
                }
              }
            }
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph2") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph2_0)
                }
              }
            },
          },
        },
      }
        "###
    );

    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on T', 'prop'],
        renameKeyTo: 'contextualArgument_2_0',
      },
    ]);
    */
    todo!("See the comment above");
}

#[test]
fn set_context_test_variable_is_a_list() {
    let (api_schema, planner) = planner!(
      Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: [String]!
        }
        type U @key(fields: "id") {
          id: ID!
          field(a: [String] @fromContext(field: "$context { prop }")): Int!
        }
        "#,
      Subgraph2: r#"
        type Query {
          a: Int!
        }
        type U @key(fields: "id") {
          id: ID!
        }
        "#
    );

    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        r#"
        {
          t {
            u {
              field
            }
          }
        }
        "#,
        "operation.graphql",
    )
    .unwrap();

    // TODO: We expect this to return an error. We should assert that the error message is what we
    // expect.
    planner
        .build_query_plan(&document, None, Default::default())
        .unwrap();
}

#[test]
fn set_context_test_fetched_as_a_list() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: [T]!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          a: Int!
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
            u {
              b
              field
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
                prop
                u {
                  __typename
                  id
                  b
                }
              }
            }
          },
          Flatten(path: "t.@.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on T', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above")
}

#[test]
fn set_context_test_impacts_on_query_planning() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: I!
        }

        interface I @context(name: "context") @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type A implements I @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type B implements I @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          a: Int!
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
            u {
              b
              field
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
                prop
                u {
                  __typename
                  id
                  b
                }
              }
            }
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on A', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
      {
        kind: 'KeyRenamer',
        path: ['..', '... on B', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above")
}

#[test]
fn set_context_test_with_type_conditions_for_union() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T!
        }

        union T @context(name: "context") = A | B

        type A @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type B @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(
            a: String
              @fromContext(
                field: "$context ... on A { prop } ... on B { prop }"
              )
          ): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          a: Int!
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
            ... on A {
              u {
                b
                field
              }
            }
            ... on B {
              u {
                b
                field
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
                ... on A {
                  __typename
                  prop
                  u {
                    __typename
                    id
                    b
                  }
                }
                ... on B {
                  __typename
                  prop
                  u {
                    __typename
                    id
                    b
                  }
                }
              }
            }
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on A', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
      {
        kind: 'KeyRenamer',
        path: ['..', '... on B', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above")
}

#[test]
fn set_context_test_accesses_a_different_top_level_query() {
    let planner = planner!(
        Subgraph1: r#"
        type Query @context(name: "topLevelQuery") {
          me: User!
          product: Product
        }

        type User @key(fields: "id") {
          id: ID!
          locale: String!
        }

        type Product @key(fields: "id") {
          id: ID!
          price(
            locale: String
              @fromContext(field: "$topLevelQuery { me { locale } }")
          ): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          randomId: ID!
        }

        type Product @key(fields: "id") {
          id: ID!
        }
        "#,
    );

    assert_plan!(planner,
        r#"
        {
          product {
            price
          }
        }
        "#,
        @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "Subgraph1") {
            {
              __typename
              me {
                locale
              }
              product {
                __typename
                id
              }
            }
          },
          Flatten(path: "product") {
            Fetch(service: "Subgraph1") {
              {
                ... on Product {
                  __typename
                  id
                }
              } =>
              {
                ... on Product {
                  price(locale: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', 'me', 'locale'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above")
}

#[test]
fn set_context_variable_is_a_list() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T!
        }
        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }
        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          randomId: ID!
        }
        "#,
    );

    assert_plan!(planner,
        r#"
        {
          t {
            u {
              field
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
                prop
                u {
                  __typename
                  id
                }
              }
            }
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on T', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above")
}

#[test]
fn set_context_required_field_is_several_levels_deep_going_back_and_forth_between_subgraphs() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T!
        }

        type A @key(fields: "id") {
          id: ID!
          b: B! @external
        }

        type B @key(fields: "id") {
          id: ID!
          c: C!
        }

        type C @key(fields: "id") {
          id: ID!
          prop: String!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          a: A!
        }
        type U @key(fields: "id") {
          id: ID!
          b: String!
          field(
            a: String @fromContext(field: "$context { a { b { c { prop }}} }")
          ): Int!
        }
        "#,
        Subgraph2: r#"
        type Query {
          randomId: ID!
        }

        type A @key(fields: "id") {
          id: ID!
          b: B!
        }

        type B @key(fields: "id") {
          id: ID!
        }
        "#,
    );

    assert_plan!(planner,
        r#"
        {
          t {
            u {
              field
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
                a {
                  __typename
                  id
                }
                u {
                  __typename
                  id
                }
              }
            }
          },
          Flatten(path: "t.a") {
            Fetch(service: "Subgraph2") {
              {
                ... on A {
                  __typename
                  id
                }
              } =>
              {
                ... on A {
                  b {
                    __typename
                    id
                  }
                }
              }
            },
          },
          Flatten(path: "t.a.b") {
            Fetch(service: "Subgraph1") {
              {
                ... on B {
                  __typename
                  id
                }
              } =>
              {
                ... on B {
                  c {
                    prop
                  }
                }
              }
            },
          },
          Flatten(path: "t.u") {
            Fetch(service: "Subgraph1") {
              {
                ... on U {
                  __typename
                  id
                }
              } =>
              {
                ... on U {
                  field(a: $contextualArgument_Subgraph1_0)
                }
              }
            },
          },
        },
      }
        "###
    );
    /* TODO: Port
    expect((plan as any).node.nodes[3].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['..', '... on T', 'a', 'b', 'c', 'prop'],
        renameKeyTo: 'contextualArgument_1_0',
      },
    ]);
    */
    todo!("See the comment above")
}

#[test]
fn set_context_test_before_key_resolution_transition() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          customer: Customer!
        }

        type Identifiers @key(fields: "id") {
          id: ID!
          legacyUserId: ID!
        }

        type Customer @key(fields: "id") {
          id: ID!
          child: Child!
          identifiers: Identifiers!
        }

        type Child @key(fields: "id") {
          id: ID!
        }
        "#,
        Subgraph2: r#"
        type Customer @key(fields: "id") @context(name: "ctx") {
          id: ID!
          identifiers: Identifiers! @external
        }

        type Identifiers @key(fields: "id") {
          id: ID!
          legacyUserId: ID! @external
        }

        type Child @key(fields: "id") {
          id: ID!
          prop(
            legacyUserId: ID
              @fromContext(field: "$ctx { identifiers { legacyUserId } }")
          ): String
        }
        "#,
    );

    assert_plan!(planner,
        r#"
        query {
          customer {
            child {
              id
              prop
            }
          }
        }
        "#,
        @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "Subgraph1") {
            {
              customer {
                __typename
                identifiers {
                  legacyUserId
                }
                child {
                  __typename
                  id
                }
              }
            }
          },
          Flatten(path: "customer.child") {
            Fetch(service: "Subgraph2") {
              {
                ... on Child {
                  __typename
                  id
                }
              } =>
              {
                ... on Child {
                  prop(legacyUserId: $contextualArgument_Subgraph2_0)
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
fn set_context_test_efficiently_merge_fetch_groups() {
    let planner = planner!(
        Subgraph1: r#"
        type Identifiers @key(fields: "id") {
          id: ID!
          id2: ID @external
          id3: ID @external
          wid: ID @requires(fields: "id2 id3")
        }
        "#,
        Subgraph2: r#"
        type Query {
          customer: Customer
        }

        type Customer @key(fields: "id") {
          id: ID!
          identifiers: Identifiers
          mid: ID
        }

        type Identifiers @key(fields: "id") {
          id: ID!
          id2: ID
          id3: ID
          id5: ID
        }
        "#,
        Subgraph3: r#"
        type Customer @key(fields: "id") @context(name: "retailCtx") {
          accounts: Accounts @shareable
          id: ID!
          mid: ID @external
          identifiers: Identifiers @external
        }

        type Identifiers @key(fields: "id") {
          id: ID!
          id5: ID @external
        }
        type Accounts @key(fields: "id") {
          foo(
            randomInput: String
            ctx_id5: ID
              @fromContext(field: "$retailCtx { identifiers { id5 } }")
            ctx_mid: ID @fromContext(field: "$retailCtx { mid }")
          ): Foo
          id: ID!
        }

        type Foo {
          id: ID
        }
        "#,
        Subgraph4: r#"
        type Customer
          @key(fields: "id", resolvable: false)
          @context(name: "widCtx") {
          accounts: Accounts @shareable
          id: ID!
          identifiers: Identifiers @external
        }

        type Identifiers @key(fields: "id", resolvable: false) {
          id: ID!
          wid: ID @external # @requires(fields: "id2 id3")
        }

        type Accounts @key(fields: "id") {
          bar(
            ctx_wid: ID @fromContext(field: "$widCtx { identifiers { wid } }")
          ): Bar

          id: ID!
        }

        type Bar {
          id: ID
        }
        "#,
    );

    assert_plan!(planner,
        r#"
        query {
          customer {
            accounts {
              foo {
                id
              }
            }
          }
        }
        "#,
        @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "Subgraph2") {
            {
              customer {
                __typename
                id
                identifiers {
                  id5
                }
                mid
              }
            }
          },
          Flatten(path: "customer") {
            Fetch(service: "Subgraph3") {
              {
                ... on Customer {
                  __typename
                  id
                }
              } =>
              {
                ... on Customer {
                  accounts {
                    foo(ctx_id5: $contextualArgument_Subgraph3_0, ctx_mid: $contextualArgument_3_1) {
                      id
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
    /* TODO: Port
    expect((plan as any).node.nodes[1].node.contextRewrites).toEqual([
      {
        kind: 'KeyRenamer',
        path: ['identifiers', 'id5'],
        renameKeyTo: 'contextualArgument_3_0',
      },
      {
        kind: 'KeyRenamer',
        path: ['mid'],
        renameKeyTo: 'contextualArgument_3_1',
      },
    ]);
    */
    todo!("See the comment above")
}
