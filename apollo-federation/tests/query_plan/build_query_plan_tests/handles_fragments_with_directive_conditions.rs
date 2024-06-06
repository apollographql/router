#[test]
#[should_panic(expected = "Subgraph unexpectedly does not use federation spec")]
// TODO: investigate this failure
fn fragment_with_intersecting_parent_type_and_directive_condition() {
    let planner = planner!(
        A: r#"
          directive @test on INLINE_FRAGMENT
          type Query {
            i: I
          }
          interface I {
            _id: ID
          }
          type T1 implements I @key(fields: "id") {
            _id: ID
            id: ID
          }
          type T2 implements I @key(fields: "id") {
            _id: ID
            id: ID
          }
        "#,
        B: r#"
          directive @test on INLINE_FRAGMENT
          type Query {
            i2s: [I2]
          }
          interface I2 {
            id: ID
            title: String
          }
          type T1 implements I2 @key(fields: "id") {
            id: ID
            title: String
          }
          type T2 implements I2 @key(fields: "id") {
            id: ID
            title: String
          }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
          query {
            i {
              _id
              ... on I2 @test {
                id
              }
            }
          }
        "#,
        @r#"
        QueryPlan {
          Fetch(service: "A") {
            {
              i {
                __typename
                _id
                ... on T1 @test {
                  id
                }
                ... on T2 @test {
                  id
                }
              }
            }
          },
        }
        "#
    );
}

#[test]
#[should_panic(expected = "Subgraph unexpectedly does not use federation spec")]
// TODO: investigate this failure
fn nested_fragment_with_interseting_parent_type_and_directive_condition() {
    let planner = planner!(
        A: r#"
          directive @test on INLINE_FRAGMENT
          type Query {
            i: I
          }
          interface I {
            _id: ID
          }
          type T1 implements I @key(fields: "id") {
            _id: ID
            id: ID
          }
          type T2 implements I @key(fields: "id") {
            _id: ID
            id: ID
          }
        "#,
        B: r#"
          directive @test on INLINE_FRAGMENT
          type Query {
            i2s: [I2]
          }
          interface I2 {
            id: ID
            title: String
          }
          type T1 implements I2 @key(fields: "id") {
            id: ID
            title: String
          }
          type T2 implements I2 @key(fields: "id") {
            id: ID
            title: String
          }
        "#,
    );

    let operation = r#"
          query {
            i {
              _id
              ... on I2 {
                ... on I2 @test {
                  id
                }
              }
            }
          }
    "#;

    //   expect(operation.expandAllFragments().toString()).toMatchInlineSnapshot(`
    //     "{
    //       i {
    //         _id
    //         ... on I2 {
    //           ... on I2 @test {
    //             id
    //           }
    //         }
    //       }
    //     }"
    //   `);
    assert_plan!(
        &planner,
        operation,
        @r#"
        QueryPlan {
          Fetch(service: "A") {
            {
              i {
                __typename
                _id
                ... on T1 {
                  ... @test {
                    id
                  }
                }
                ... on T2 {
                  ... @test {
                    id
                  }
                }
              }
            }
          },
        }
        "#
    );
}
