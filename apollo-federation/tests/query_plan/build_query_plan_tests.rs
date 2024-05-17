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

mod shareable_root_fields;

// TODO: port the rest of query-planner-js/src/__tests__/buildPlan.test.ts
