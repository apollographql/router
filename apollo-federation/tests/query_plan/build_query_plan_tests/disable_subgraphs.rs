use apollo_federation::error::FederationError;
use apollo_federation::error::SingleFederationError;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;

const SUBGRAPH_A: &str = r#"
  type Query {
    foo: Foo
  }

  type Foo {
    idA: ID! @shareable
    idB: ID! @shareable
  }
"#;

const SUBGRAPH_B: &str = r#"
  type Foo @key(fields: "idA idB") {
    idA: ID!
    idB: ID!
    bar: String! @shareable
  }
"#;

const SUBGRAPH_C: &str = r#"
  type Foo @key(fields: "idA") {
    idA: ID!
    bar: String! @shareable
  }
"#;

const OPERATION: &str = r#"
  query {
    foo {
      bar
    }
  }
"#;

#[test]
fn test_if_less_expensive_subgraph_jump_is_used() {
    let planner = planner!(
        subgraphA: SUBGRAPH_A,
        subgraphB: SUBGRAPH_B,
        subgraphC: SUBGRAPH_C,
    );
    assert_plan!(
      &planner,
      OPERATION,
      @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "subgraphA") {
            {
              foo {
                __typename
                idA
              }
            }
          },
          Flatten(path: "foo") {
            Fetch(service: "subgraphC") {
              {
                ... on Foo {
                  __typename
                  idA
                }
              } =>
              {
                ... on Foo {
                  bar
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
fn test_if_disabling_less_expensive_subgraph_jump_causes_other_to_be_used() {
    // setup_tracing_subscriber().expect("Failed to setup tracing");
    let planner = planner!(
        subgraphA: SUBGRAPH_A,
        subgraphB: SUBGRAPH_B,
        subgraphC: SUBGRAPH_C,
    );
    assert_plan!(
      &planner,
      OPERATION,
      QueryPlanOptions {
          disabled_subgraph_names: vec!["subgraphC".to_string()].into_iter().collect(),
          ..Default::default()
      },
      @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "subgraphA") {
            {
              foo {
                __typename
                idA
                idB
              }
            }
          },
          Flatten(path: "foo") {
            Fetch(service: "subgraphB") {
              {
                ... on Foo {
                  __typename
                  idA
                  idB
                }
              } =>
              {
                ... on Foo {
                  bar
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
fn test_if_disabling_all_subgraph_jumps_causes_error() {
    let planner = planner!(
        subgraphA: SUBGRAPH_A,
        subgraphB: SUBGRAPH_B,
        subgraphC: SUBGRAPH_C,
    );
    let api_schema = planner.api_schema();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        OPERATION,
        "operation.graphql",
    )
    .expect("valid graphql document");
    assert!(matches!(
        planner
            .build_query_plan(
                &document,
                None,
                QueryPlanOptions {
                    disabled_subgraph_names: vec!["subgraphB".to_string(), "subgraphC".to_string()]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            )
            .err(),
        Some(FederationError::SingleFederationError(
            SingleFederationError::NoPlanFoundWithDisabledSubgraphs
        ))
    ))
}
