use apollo_compiler::collections::HashSet;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;

mod shareable;

#[test]
fn it_handles_progressive_override_on_root_fields() {
    let planner = planner!(
        s1: r#"
          type Query {
            hello: String
          }
        "#,
        s2: r#"
          type Query {
            hello: String @override(from: "s1", label: "test")
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            hello
          }
        "#,
        QueryPlanOptions {
            override_conditions: HashSet::from_iter(["test".to_string()])
        },
        @r###"
        QueryPlan {
          Fetch(service: "s2") {
            {
              hello
            }
          },
        }
      "###
    );
}

#[test]
fn it_does_not_override_unset_labels_on_root_fields() {
    let planner = planner!(
        s1: r#"
          type Query {
            hello: String
          }
        "#,
        s2: r#"
          type Query {
            hello: String @override(from: "s1", label: "test")
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            hello
          }
        "#,

        @r###"
        QueryPlan {
          Fetch(service: "s1") {
            {
              hello
            }
          },
        }
      "###
    );
}

#[test]
fn it_handles_progressive_override_on_entity_fields() {
    let planner = planner!(
        s1: r#"
          type Query {
            t: T
            t2: T2
          }

          type T @key(fields: "id") {
            id: ID!
            f1: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s2", label: "test2")
            t: T
          }
        "#,
        s2: r#"
          type T @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s1", label: "test")
            f2: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String
            f2: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              f1
              f2
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "s1") {
                {
                  t {
                    __typename
                    id
                  }
                }
              },
              Flatten(path: "t") {
                Fetch(service: "s2") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      f1
                      f2
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
fn it_does_not_override_unset_labels_on_entity_fields() {
    let planner = planner!(
        s1: r#"
          type Query {
            t: T
            t2: T2
          }

          type T @key(fields: "id") {
            id: ID!
            f1: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s2", label: "test2")
            t: T
          }
        "#,
        s2: r#"
          type T @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s1", label: "test")
            f2: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String
            f2: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              f1
              f2
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "s1") {
                {
                  t {
                    __typename
                    id
                    f1
                  }
                }
              },
              Flatten(path: "t") {
                Fetch(service: "s2") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      f2
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
fn it_handles_progressive_override_on_nested_entity_fields() {
    let planner = planner!(
        s1: r#"
          type Query {
            t: T
            t2: T2
          }

          type T @key(fields: "id") {
            id: ID!
            f1: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s2", label: "test2")
            t: T
          }
        "#,
        s2: r#"
          type T @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s1", label: "test")
            f2: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String
            f2: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t2 {
              t {
                f1
              }
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "s1") {
                {
                  t2 {
                    t {
                      __typename
                      id
                    }
                  }
                }
              },
              Flatten(path: "t2.t") {
                Fetch(service: "s2") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      f1
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
fn it_does_not_override_unset_labels_on_nested_entity_fields() {
    let planner = planner!(
        s1: r#"
          type Query {
            t: T
            t2: T2
          }

          type T @key(fields: "id") {
            id: ID!
            f1: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s2", label: "test2")
            t: T
          }
        "#,
        s2: r#"
          type T @key(fields: "id") {
            id: ID!
            f1: String @override(from: "s1", label: "test")
            f2: String
          }

          type T2 @key(fields: "id") {
            id: ID!
            f1: String
            f2: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t2 {
              t {
                f1
              }
            }
          }
        "#,

        @r###"
          QueryPlan {
            Fetch(service: "s1") {
              {
                t2 {
                  t {
                    f1
                  }
                }
              }
            },
          }
      "###
    );
}
