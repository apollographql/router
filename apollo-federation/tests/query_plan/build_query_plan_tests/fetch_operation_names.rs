use std::fmt::Write;

use apollo_federation::query_plan::query_planner::QueryPlannerDebugConfig;
use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::TopLevelPlanNode;

fn second_operation(plan: &QueryPlan) -> String {
    let Some(TopLevelPlanNode::Sequence(node)) = &plan.node else {
        panic!()
    };
    let [_, PlanNode::Flatten(node)] = &*node.nodes else {
        panic!()
    };
    let PlanNode::Fetch(node) = &*node.node else {
        panic!()
    };
    node.operation_document.to_string()
}

macro_rules! assert_starts_with {
    ($haystack: expr, $needle: expr) => {{
        let haystack = $haystack;
        let needle = $needle;
        assert!(
            haystack.starts_with(needle),
            "{:?}.starts_with({needle:?})",
            &haystack[..50]
        );
    }};
}

#[test]
fn handle_subgraph_with_hypen_in_the_name() {
    let planner = planner!(
        S1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        "non-graphql-name": r#"
          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          query myOp {
            t {
              x
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
              Fetch(service: "non-graphql-name") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    x
                  }
                }
              },
            },
          },
        }
      "###
    );
    assert_starts_with!(second_operation(&plan), "query myOp__non_graphql_name__1(");
}

#[test]
fn ensures_sanitization_applies_repeatedly() {
    let planner = planner!(
        S1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        "a-na&me-with-plen&ty-replace*ments": r#"
          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          query myOp {
            t {
              x
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
              Fetch(service: "a-na&me-with-plen&ty-replace*ments") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    x
                  }
                }
              },
            },
          },
        }
      "###
    );
    assert_starts_with!(
        second_operation(&plan),
        "query myOp__a_name_with_plenty_replacements__1("
    );
}

#[test]
fn handle_very_non_graph_subgraph_name() {
    let planner = planner!(
        S1: r#"
          type Query {
            t: T
          }
  
          type T @key(fields: "id") {
            id: ID!
          }
        "#,
        "42!": r#"
          type T @key(fields: "id") {
            id: ID!
            x: Int
          }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          query myOp {
            t {
              x
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
              Fetch(service: "42!") {
                {
                  ... on T {
                    __typename
                    id
                  }
                } =>
                {
                  ... on T {
                    x
                  }
                }
              },
            },
          },
        }
      "###
    );
    assert_starts_with!(second_operation(&plan), "query myOp___42__1(");
}

#[test]
fn correctly_handle_case_where_there_is_too_many_plans_to_consider() {
    // Creating realistic examples where there is too many plan to consider is not trivial, but creating unrealistic examples
    // is thankfully trivial. Here, we just have 2 subgraphs that are _exactly the same_ with a single type having plenty of
    // fields. The reason this create plenty of possible query plans is that each field can be independently reached
    // from either subgraph and so in theory the possible plans is the cartesian product of the 2 choices for each field (which
    // gets very large very quickly). Obviously, there is no reason to do this in practice.

    // Each leaf field is reachable from 2 subgraphs, so doubles the number of plans.
    let default_max_computed_plans = QueryPlannerDebugConfig::default().max_evaluated_plans.get();
    let field_count = (default_max_computed_plans as f64).log2().ceil() as usize + 1;
    let mut field_names: Vec<_> = (0..field_count).map(|i| format!("f{i}")).collect();
    let mut schema = r#"
      type Query {
        t: T @shareable
      }

      type T {"#
        .to_owned();
    let mut operation = "{\n  t {".to_owned();
    for f in &field_names {
        write!(&mut schema, "\n        {f}: Int @shareable").unwrap();
        write!(&mut operation, "\n    {f}").unwrap();
    }
    schema.push_str("\n      }\n");
    operation.push_str("\n  }\n}\n");

    let (api_schema, planner) = planner!(
        S1: &schema,
        S2: &schema,
    );
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        operation,
        "operation.graphql",
    )
    .unwrap();
    let plan = planner.build_query_plan(&document, None, None).unwrap();

    // Note: The way the code that handle multiple plans currently work, it mess up the order of fields a bit. It's not a
    // big deal in practice cause everything gets re-order in practice during actual execution, but this means it's a tad
    // harder to valid the plan automatically here with `toMatchInlineSnapshot`.

    let Some(TopLevelPlanNode::Fetch(fetch)) = &plan.node else {
        panic!()
    };
    assert_eq!(fetch.subgraph_name.as_ref(), "S1");
    assert!(fetch.requires.is_none());
    assert!(fetch.operation_document.fragments.is_empty());
    let mut operations = fetch.operation_document.operations.iter();
    let operation = operations.next().unwrap();
    assert!(operations.next().is_none());
    // operation is essentially:
    // {
    //   t {
    //     ... all fields
    //   }
    // }
    assert_eq!(operation.selection_set.selections.len(), 1);
    let field = operation.selection_set.selections[0].as_field().unwrap();
    let mut names: Vec<_> = field
        .selection_set
        .selections
        .iter()
        .map(|sel| sel.as_field().unwrap().name.as_str().to_owned())
        .collect();
    names.sort();
    field_names.sort();
    assert_eq!(names, field_names);
}
