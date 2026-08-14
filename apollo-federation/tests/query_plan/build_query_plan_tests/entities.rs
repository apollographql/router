// TODO this test shows inefficient QP where we make multiple parallel
// fetches of the same entity from the same subgraph but for different paths
#[test]
fn inefficient_entity_fetches_to_same_subgraph() {
    let planner = planner!(
        Subgraph1: r#"
          type V @shareable {
            x: Int
          }

          interface I {
            v: V
          }

          type Outer implements I @key(fields: "id") {
            id: ID!
            v: V
          }
        "#,
        Subgraph2: r#"
          type Query {
            outer1: Outer
            outer2: Outer
          }

          type V @shareable {
            x: Int
          }

          interface I {
            v: V
            w: Int
          }

          type Inner implements I {
            v: V
            w: Int
          }

          type Outer @key(fields: "id") {
            id: ID!
            inner: Inner
            w: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query {
            outer1 {
              ...OuterFrag
            }
            outer2 {
              ...OuterFrag
            }
          }

          fragment OuterFrag on Outer {
            ...IFrag
            inner {
              ...IFrag
            }
          }

          fragment IFrag on I {
            v {
              x
            }
            w
          }
        "#,
        @r#"
        QueryPlan {
          Sequence {
            Fetch(service: "Subgraph2") {
              {
                outer1 {
                  __typename
                  id
                  w
                  inner {
                    v {
                      x
                    }
                    w
                  }
                }
                outer2 {
                  __typename
                  id
                  w
                  inner {
                    v {
                      x
                    }
                    w
                  }
                }
              }
            },
            Parallel {
              Flatten(path: "outer2") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
                      }
                    }
                  }
                },
              },
              Flatten(path: "outer1") {
                Fetch(service: "Subgraph1") {
                  {
                    ... on Outer {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on Outer {
                      v {
                        x
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
fn repeated_planning_produces_identical_entity_type_condition_batches() {
    // The repeated requirements create duplicate mergeable fetches at both `v` and `w`, while
    // the four interface implementations keep the downstream entity fetches type-conditioned.
    let planner = planner!(
        s1: r#"
          type Query { values: [I!]! }

          interface I { id: ID! }
          type A implements I @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type B implements I @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type C implements I @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type D implements I @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type V @key(fields: "id") @key(fields: "internalID") { id: ID!, internalID: ID! }
          type W @key(fields: "id") @key(fields: "internalID") { id: ID!, internalID: ID! }
          type X @shareable { isX: Boolean! }
        "#,
        s2: r#"
          type V @key(fields: "id") {
            id: ID!
            internalID: ID! @shareable
            y: Y! @shareable
            zz: [Z!] @external
          }
          type W @key(fields: "id") {
            id: ID!
            internalID: ID! @shareable
            y: Y! @shareable
            zz: [Z!] @external
          }
          type Z { u: U! @external }
          type Y @key(fields: "id") { id: ID!, isY: Boolean! @external }
          type X { isX: Boolean! @external }
          interface Out { id: ID!, name: String! }
          type U implements Out @key(fields: "id") { id: ID!, name: String! @external }
          type A @key(fields: "id") { id: ID!, x: X @external, v: V @external, w: W @external, foo: String @requires(fields: "x { isX } v { y { isY } } w { y { isY } }"), bar: [Out!]! @requires(fields: "x { isX } v { y { isY } zz { u { id } } } w { y { isY } zz { u { id } } }") }
          type B @key(fields: "id") { id: ID!, x: X @external, v: V @external, w: W @external, foo: String @requires(fields: "x { isX } v { y { isY } } w { y { isY } }"), bar: [Out!]! @requires(fields: "x { isX } v { y { isY } zz { u { id } } } w { y { isY } zz { u { id } } }") }
          type C @key(fields: "id") { id: ID!, x: X @external, v: V @external, w: W @external, foo: String @requires(fields: "x { isX } v { y { isY } } w { y { isY } }"), bar: [Out!]! @requires(fields: "x { isX } v { y { isY } zz { u { id } } } w { y { isY } zz { u { id } } }") }
          type D @key(fields: "id") { id: ID!, x: X @external, v: V @external, w: W @external, foo: String @requires(fields: "x { isX } v { y { isY } } w { y { isY } }"), bar: [Out!]! @requires(fields: "x { isX } v { y { isY } zz { u { id } } } w { y { isY } zz { u { id } } }") }
        "#,
        s3: r#"
          type V @key(fields: "internalID") { internalID: ID!, y: Y! @shareable }
          type W @key(fields: "internalID") { internalID: ID!, y: Y! @shareable }
          type Y @key(fields: "id") { id: ID!, isY: Boolean! }
        "#,
        s4: r#"
          type V @key(fields: "id") @key(fields: "internalID") { id: ID!, internalID: ID!, zz: [Z!] }
          type W @key(fields: "id") @key(fields: "internalID") { id: ID!, internalID: ID!, zz: [Z!] }
          type Z { u: U!, v: V! }
          type A @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type B @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type C @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type D @key(fields: "id") { id: ID!, x: X @shareable, v: V @shareable, w: W @shareable }
          type X @shareable { isX: Boolean! }
          interface Out { id: ID!, name: String! }
          type U implements Out @key(fields: "id") { id: ID!, name: String! }
        "#,
    );
    let operation = r#"
      {
        values {
          ... on A { foo bar { name } }
          ... on B { foo bar { name } }
          ... on C { foo bar { name } }
          ... on D { foo bar { name } }
        }
      }
    "#;
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        operation,
        "operation.graphql",
    )
    .expect("valid operation");
    let expected = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan generated")
        .to_string();

    for call in 2..=64 {
        let actual = planner
            .build_query_plan(&document, None, Default::default())
            .expect("query plan generated")
            .to_string();
        assert_eq!(
            actual, expected,
            "query plan changed on planning call {call}"
        );
    }
}
