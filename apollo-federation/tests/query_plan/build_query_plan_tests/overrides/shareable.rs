const S1: &str = r#"
  type Query {
    t: T
  }

  type T @key(fields: "id") {
    id: ID!
    f1: String @shareable
  }
"#;

const S2: &str = r#"
  type T @key(fields: "id") {
    id: ID!
    f1: String @shareable @override(from: "S1", label: "test")
    f2: String
  }
"#;

const S3: &str = r#"
  type T @key(fields: "id") {
    id: ID!
    f1: String @shareable
    f3: String
  }
"#;

#[test]
fn it_overrides_to_s2_when_label_is_provided() {
    let planner = planner!(
        S1: S1,
        S2: S2,
        S3: S3,
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
              Fetch(service: "S1") {
                {
                  t {
                    __typename
                    id
                  }
                }
              },
              Flatten(path: "t") {
                Fetch(service: "S2") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      f2
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
fn it_resolves_in_s1_when_label_is_not_provided() {
    let planner = planner!(
        S1: S1,
        S2: S2,
        S3: S3,
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
              Fetch(service: "S1") {
                {
                  t {
                    __typename
                    id
                    f1
                  }
                }
              },
              Flatten(path: "t") {
                Fetch(service: "S2") {
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

// This is very similar to the S2 example. The fact that the @override in S2
// specifies _from_ S1 actually affects all T.f1 fields the same way (except
// S1). That is to say, it's functionally equivalent to have the `@override`
// exist in either S2 or S3 from S2/S3/Sn's perspective. It's helpful to
// test here that the QP will take a path through _either_ S2 or S3 when
// appropriate to do so. In these tests and the previous S2 tests,
// "appropriate" is determined by the other fields being selected in the
// query.
#[test]
fn it_overrides_f1_to_s3_when_label_is_provided() {
    let planner = planner!(
        S1: S1,
        S2: S2,
        S3: S3,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              f1
              f3
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S1") {
                {
                  t {
                    __typename
                    id
                  }
                }
              },
              Flatten(path: "t") {
                Fetch(service: "S3") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      f1
                      f3
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
fn it_resolves_f1_in_s1_when_label_is_not_provided() {
    let planner = planner!(
        S1: S1,
        S2: S2,
        S3: S3,
    );
    assert_plan!(
        &planner,
        r#"
          {
            t {
              f1
              f3
            }
          }
        "#,

        @r###"
          QueryPlan {
            Sequence {
              Fetch(service: "S1") {
                {
                  t {
                    __typename
                    id
                    f1
                  }
                }
              },
              Flatten(path: "t") {
                Fetch(service: "S3") {
                  {
                    ... on T {
                      __typename
                      id
                    }
                  } =>
                  {
                    ... on T {
                      f3
                    }
                  }
                },
              },
            },
          }
      "###
    );
}
