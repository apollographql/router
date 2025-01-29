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
fn it_preserves_directives() {
    // (because used only once)
    let planner = planner!(
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
    );
    assert_plan!(
        &planner,
        r#"
          query test($if: Boolean!) {
            t {
              id
              ...OnT @include(if: $if)
            }
          }

          fragment OnT on T {
            a
            b
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                id
                ... on T @include(if: $if) {
                  a
                  b
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn it_preserves_directives_when_fragment_is_reused() {
    let planner = planner!(
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
    );
    assert_plan!(
        &planner,
        r#"
          query test($test1: Boolean!, $test2: Boolean!) {
            t {
              id
              ...OnT @include(if: $test1)
              ...OnT @include(if: $test2)
            }
          }

          fragment OnT on T {
            a
            b
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                id
                ... on T @include(if: $test1) {
                  a
                  b
                }
                ... on T @include(if: $test2) {
                  a
                  b
                }
              }
            }
          },
        }
      "###
    );
}

#[test]
fn it_preserves_directives_on_collapsed_fragments() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T {
            id: ID!
            t1: V
            t2: V
          }

          type V {
            v1: Int
            v2: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query($test: Boolean!) {
            t {
              ...OnT
            }
          }

          fragment OnT on T {
            id
            ...OnTInner @include(if: $test)
          }

          fragment OnTInner on T {
            t1 {
              ...OnV
            }
            t2 {
              ...OnV
            }
          }

          fragment OnV on V {
            v1
            v2
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                id
                ... on T @include(if: $test) {
                  t1 {
                    v1
                    v2
                  }
                  t2 {
                    v1
                    v2
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
fn it_expands_nested_fragments() {
    let planner = planner!(
        Subgraph1: r#"
          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            a: V
            b: V
          }

          type V {
            v1: Int
            v2: Int
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              ...OnT
            }
          }

          fragment OnT on T {
            a {
              ...OnV
            }
            b {
              ...OnV
            }
          }

          fragment OnV on V {
            v1
            v2
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                a {
                  v1
                  v2
                }
                b {
                  v1
                  v2
                }
              }
            }
          },
        }
      "###
    );
}
