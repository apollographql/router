#[test]
fn merging_skip_and_include_directives_with_fragment() {
    let planner = planner!(
        SubgraphSkip: r#"
          type Query {
              hello: Hello!
              extraFieldToPreventSkipIncludeNodes: String!
          }

          type Hello {
              world: String!
              goodbye: String!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Test($skipField: Boolean!) {
            ...ConditionalSkipFragment
            hello {
              world
            }
            extraFieldToPreventSkipIncludeNodes
          }

          fragment ConditionalSkipFragment on Query {
            hello @skip(if: $skipField) {
              goodbye
            }
          }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "SubgraphSkip") {
              {
                hello @skip(if: $skipField) {
                  goodbye
                }
                hello {
                  world
                }
                extraFieldToPreventSkipIncludeNodes
              }
            },
          }
        "###
    );
}

#[test]
fn merging_skip_and_include_directives_without_fragment() {
    let planner = planner!(
        SubgraphSkip: r#"
          type Query {
              hello: Hello!
              extraFieldToPreventSkipIncludeNodes: String!
          }

          type Hello {
              world: String!
              goodbye: String!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Test($skipField: Boolean!) {
            hello @skip(if: $skipField) {
              world
            }
            hello {
              goodbye
            }
            extraFieldToPreventSkipIncludeNodes
          }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "SubgraphSkip") {
              {
                hello @skip(if: $skipField) {
                  world
                }
                hello {
                  goodbye
                }
                extraFieldToPreventSkipIncludeNodes
              }
            },
          }
        "###
    );
}

#[test]
fn merging_skip_and_include_directives_multiple_applications_identical() {
    let planner = planner!(
        SubgraphSkip: r#"
          type Query {
              hello: Hello!
              extraFieldToPreventSkipIncludeNodes: String!
          }

          type Hello {
              world: String!
              goodbye: String!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Test($skipField: Boolean!, $includeField: Boolean!) {
            hello @skip(if: $skipField) @include(if: $includeField) {
              world
            }
            hello @skip(if: $skipField) @include(if: $includeField) {
              goodbye
            }
            extraFieldToPreventSkipIncludeNodes
          }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "SubgraphSkip") {
              {
                hello @skip(if: $skipField) @include(if: $includeField) {
                  world
                  goodbye
                }
                extraFieldToPreventSkipIncludeNodes
              }
            },
          }
        "###
    );
}

#[test]
fn merging_skip_and_include_directives_multiple_applications_differing_order() {
    let planner = planner!(
        SubgraphSkip: r#"
          type Query {
              hello: Hello!
              extraFieldToPreventSkipIncludeNodes: String!
          }

          type Hello {
              world: String!
              goodbye: String!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Test($skipField: Boolean!, $includeField: Boolean!) {
            hello @skip(if: $skipField) @include(if: $includeField) {
              world
            }
            hello @include(if: $includeField) @skip(if: $skipField) {
              goodbye
            }
            extraFieldToPreventSkipIncludeNodes
          }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "SubgraphSkip") {
              {
                hello @skip(if: $skipField) @include(if: $includeField) {
                  world
                  goodbye
                }
                extraFieldToPreventSkipIncludeNodes
              }
            },
          }
        "###
    );
}

#[test]
fn merging_skip_and_include_directives_multiple_applications_differing_quantity() {
    let planner = planner!(
        SubgraphSkip: r#"
          type Query {
              hello: Hello!
              extraFieldToPreventSkipIncludeNodes: String!
          }

          type Hello {
              world: String!
              goodbye: String!
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Test($skipField: Boolean!, $includeField: Boolean!) {
            hello @skip(if: $skipField) @include(if: $includeField) {
              world
            }
            hello @include(if: $includeField) {
              goodbye
            }
            extraFieldToPreventSkipIncludeNodes
          }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "SubgraphSkip") {
              {
                hello @skip(if: $skipField) @include(if: $includeField) {
                  world
                }
                hello @include(if: $includeField) {
                  goodbye
                }
                extraFieldToPreventSkipIncludeNodes
              }
            },
          }
        "###
    );
}

#[test]
fn fields_are_not_overwritten_when_directives_are_removed() {
    let planner = planner!(
        SubgraphSkip: r#"
          type Query {
            foo: Foo
          }

          type Foo {
            bar: Bar
          }

          type Bar {
            things: String
            name: String
          }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
          query Test($b: Boolean!) {
            foo @include(if: $b) {
              bar {
                name
              }
              bar @include(if: $b) {
                things
              }
            }
          }
        "#,
        @r###"
          QueryPlan {
            Include(if: $b) {
              Fetch(service: "SubgraphSkip") {
                {
                  foo {
                    bar {
                      name
                      things
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
          query Test($b: Boolean!) {
            foo @skip(if: $b) {
              bar {
                name
              }
              bar @skip(if: $b) {
                things
              }
            }
          }
        "#,
        @r###"
          QueryPlan {
            Skip(if: $b) {
              Fetch(service: "SubgraphSkip") {
                {
                  foo {
                    bar {
                      name
                      things
                    }
                  }
                }
              },
            },
          }
        "###
    );
}
