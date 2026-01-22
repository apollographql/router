use apollo_router::TestHarness;
use apollo_router::services::supergraph;
use tokio_stream::StreamExt;
use tower::BoxError;
use tower::ServiceExt;

macro_rules! set_snapshot_suffix {
    ($($expr:expr),*) => {
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!($($expr,)*));
        settings.set_sort_maps(true);
        let _guard = settings.bind_to_scope();
    }
}

struct TestSetupParameters {
    name: &'static str,
    query: &'static str,
    schema: &'static str,
    subgraphs: serde_json::Value,
}

#[rstest::fixture]
fn basic_fragments() -> TestSetupParameters {
    TestSetupParameters {
        name: "basic_fragments",
        query: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/basic_fragments_query.graphql"
        ),
        schema: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/basic_supergraph_schema.graphql"
        ),
        subgraphs: serde_json::json!({
            "products": {
                "query": {
                    "interfaceInstance1": {"__typename": "SecondObjectType", "field1": null, "field2": "hello"},
                    "someUnion": {"__typename": "FirstObjectType", "innerList": []}
                },
            }
        }),
    }
}

#[rstest::fixture]
fn basic_mutation() -> TestSetupParameters {
    TestSetupParameters {
        name: "basic_mutation",
        query: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/basic_mutation.graphql"
        ),
        schema: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/basic_supergraph_schema.graphql"
        ),
        subgraphs: serde_json::json!({
            "products": {
                "mutation": {
                    "doSomething": 6,
                },
            }
        }),
    }
}

#[rstest::fixture]
fn federated_ships_required() -> TestSetupParameters {
    TestSetupParameters {
        name: "federated_ships_required",
        query: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/federated_ships_required_query.graphql"
        ),
        schema: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        ),
        subgraphs: serde_json::json!({
            "vehicles": {
                "query": {
                    "ships": [
                        {"__typename": "Ship", "id": 1, "name": "Ship1", "owner": {"__typename": "User", "licenseNumber": 10}},
                        {"__typename": "Ship", "id": 2, "name": "Ship2", "owner": {"__typename": "User", "licenseNumber": 11}},
                        {"__typename": "Ship", "id": 3, "name": "Ship3", "owner": {"__typename": "User", "licenseNumber": 12}},
                    ],
                },
                "entities": [
                    {"__typename": "Ship", "id": 1, "owner": {"addresses": [{"zipCode": 18263}]}, "registrationFee": 129.2},
                    {"__typename": "Ship", "id": 2, "owner": {"addresses": [{"zipCode": 61027}]}, "registrationFee": 14.0},
                    {"__typename": "Ship", "id": 3, "owner": {"addresses": [{"zipCode": 86204}]}, "registrationFee": 97.15},
                ]
            },
            "users": {
                "entities": [
                    {"__typename": "User", "licenseNumber": 10, "addresses": [{"zipCode": 18263}]},
                    {"__typename": "User", "licenseNumber": 11, "addresses": [{"zipCode": 61027}]},
                    {"__typename": "User", "licenseNumber": 12, "addresses": [{"zipCode": 86204}]},
                ],
            }
        }),
    }
}

#[rstest::fixture]
fn custom_costs() -> TestSetupParameters {
    TestSetupParameters {
        name: "custom_costs",
        query: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/custom_cost_query.graphql"
        ),
        schema: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/custom_cost_schema.graphql"
        ),
        subgraphs: serde_json::json!({
            "subgraphWithCost": {
                "query": {
                    "fieldWithCost": 2,
                    "argWithCost": 30,
                    "enumWithCost": "A",
                    "inputWithCost": 5,
                    "scalarWithCost": 6172364,
                    "objectWithCost": {"id": 9},
                },
            },
            "subgraphWithListSize": {
                "query": {
                    "fieldWithListSize": ["hello", "world", "and", "nearby", "planets"],
                    "fieldWithDynamicListSize": {"items": [{"id": 7}, {"id": 9}]},
                },
            }
        }),
    }
}

async fn query_supergraph_service(
    test_parameters: TestSetupParameters,
    demand_control: serde_json::Value,
) -> Result<supergraph::Response, BoxError> {
    let service = TestHarness::builder()
        .schema(test_parameters.schema)
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": {"all": true},
            "demand_control": demand_control,
            "experimental_mock_subgraphs": test_parameters.subgraphs,
        }))?
        .build_supergraph()
        .await?;

    let request = supergraph::Request::fake_builder()
        .query(test_parameters.query)
        .build()?;
    service.oneshot(request).await
}

async fn parse_result_for_snapshot(response: supergraph::Response) -> serde_json::Value {
    let context = response.context;
    let body = response.response.into_body().next().await.unwrap();

    let mut result = serde_json::json!({"body": body});
    for field in [
        "apollo::demand_control::actual_cost",
        "apollo::demand_control::actual_cost_by_subgraph",
        "apollo::demand_control::estimated_cost",
        "apollo::demand_control::result",
        "apollo::demand_control::strategy",
        "apollo::experimental_mock_subgraphs::subgraph_call_count",
    ] {
        let value: Option<serde_json::Value> = context.get(field).expect("can't deserialize");
        result
            .as_object_mut()
            .unwrap()
            .insert(field.to_string(), value.unwrap_or_default());
    }

    result
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
#[case::eq(basic_fragments(), 12.0)]
#[case::lt(basic_fragments(), 15.0)]
#[case::eq(basic_mutation(), 10.0)]
#[case::lt(basic_mutation(), 15.0)]
#[case::eq(federated_ships_required(), 140.0)]
#[case::lt(federated_ships_required(), 150.0)]
#[case::eq(custom_costs(), 127.0)]
#[case::lt(custom_costs(), 130.0)]
async fn requests_within_max_are_accepted(
    #[case] test_parameters: TestSetupParameters,
    #[case] max_cost: f64,
) -> Result<(), BoxError> {
    set_snapshot_suffix!("{}_{}", test_parameters.name, max_cost);

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": 10,
                "max": max_cost
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
async fn requests_exceeding_max_are_rejected(
    #[values(
        basic_fragments(),
        basic_mutation(),
        federated_ships_required(),
        custom_costs()
    )]
    test_parameters: TestSetupParameters,
) -> Result<(), BoxError> {
    set_snapshot_suffix!("{}", test_parameters.name);

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": 100,
                "max": 1.0
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
async fn actual_cost_can_vary_based_on_mode(
    #[values(
        basic_fragments(),
        basic_mutation(),
        federated_ships_required(),
        custom_costs()
    )]
    test_parameters: TestSetupParameters,
    #[values("by_subgraph", "legacy")] mode: &str,
) -> Result<(), BoxError> {
    set_snapshot_suffix!("{}_{}", test_parameters.name, mode);

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": 10,
                "actual_cost_mode": mode,
                "max": 10000000
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}
