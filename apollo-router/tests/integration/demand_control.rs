use apollo_router::TestHarness;
use apollo_router::services::supergraph;
use tokio_stream::StreamExt;
use tower::BoxError;
use tower::ServiceExt;
use wiremock::ResponseTemplate;

// Reasonable default max that should not be exceeded by any of these tests.
const MAX_COST: f64 = 10_000_000.0;

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
                    {"__typename": "Ship", "id": 1, "owner": null, "registrationFee": null},
                    {"__typename": "Ship", "id": 2, "owner": null, "registrationFee": null},
                    {"__typename": "Ship", "id": 3, "owner": null, "registrationFee": null},
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
fn federated_ships_fragment() -> TestSetupParameters {
    TestSetupParameters {
        name: "federated_ships_fragment",
        query: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql"
        ),
        schema: include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        ),
        subgraphs: serde_json::json!({
            "vehicles": {
                "query": {
                    "ships": [
                        {"__typename": "Ship", "id": 1, "name": "Ship1", "owner": {"__typename": "User", "licenseNumber": 100}},
                        {"__typename": "Ship", "id": 2, "name": "Ship2", "owner": {"__typename": "User", "licenseNumber": 110}},
                        {"__typename": "Ship", "id": 3, "name": "Ship3", "owner": {"__typename": "User", "licenseNumber": 120}},
                        {"__typename": "Ship", "id": 4, "name": "Ship4", "owner": {"__typename": "User", "licenseNumber": 120}},
                        {"__typename": "Ship", "id": 5, "name": "Ship5", "owner": {"__typename": "User", "licenseNumber": 120}},
                    ],
                },
            },
            "users": {
                "query": {
                    "users": [
                        {"__typename": "User", "name": "User10", "licenseNumber": 10},
                        {"__typename": "User", "name": "User11", "licenseNumber": 11},
                    ]
                },
                "entities": [
                    {"__typename": "User", "name": "User100", "licenseNumber": 100},
                    {"__typename": "User", "name": "User110", "licenseNumber": 110},
                    {"__typename": "User", "name": "User120", "licenseNumber": 120},
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
        "apollo::demand_control::estimated_cost_by_subgraph",
        "apollo::demand_control::result",
        "apollo::demand_control::result_by_subgraph",
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
#[case::eq(federated_ships_fragment(), 40.0)]
#[case::lt(federated_ships_fragment(), 50.0)]
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
        federated_ships_fragment(),
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
        federated_ships_fragment(),
        custom_costs()
    )]
    test_parameters: TestSetupParameters,
    #[values("by_subgraph", "response_shape")] mode: &str,
) -> Result<(), BoxError> {
    set_snapshot_suffix!("{}_{}", test_parameters.name, mode);

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": 10,
                "actual_cost_mode": mode,
                "max": MAX_COST
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
async fn requests_exceeding_max_are_rejected_regardless_of_subgraph_config(
    #[values(
        basic_fragments(),
        basic_mutation(),
        federated_ships_required(),
        federated_ships_fragment(),
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
                "list_size": 10,
                "max": 1.0,
                "subgraph": {
                    "all": {
                        "max": MAX_COST
                    }
                }
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
#[case(basic_fragments(), serde_json::json!({"products": {"max": 1.0}}))]
#[case(basic_mutation(), serde_json::json!({"products": {"max": 1.0}}))]
#[case(federated_ships_required(), serde_json::json!({"users": {"max": 1.0}}))]
#[case(federated_ships_fragment(), serde_json::json!({"vehicles": {"max": 1.0}}))]
#[case(custom_costs(), serde_json::json!({"subgraphWithListSize": {"max": 1.0}}))]
async fn requests_exceeding_one_subgraph_cost_are_accepted(
    #[case] test_parameters: TestSetupParameters,
    #[case] subgraphs: serde_json::Value,
) -> Result<(), BoxError> {
    set_snapshot_suffix!("{}", test_parameters.name);

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": 10,
                "max": MAX_COST,
                "subgraph": {
                    // no `all` value
                    "subgraphs": subgraphs
                }
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
async fn requests_exceeding_max_are_not_rejected_in_measure_mode(
    #[values(
        basic_fragments(),
        basic_mutation(),
        federated_ships_required(),
        federated_ships_fragment(),
        custom_costs()
    )]
    test_parameters: TestSetupParameters,
) -> Result<(), BoxError> {
    set_snapshot_suffix!("{}", test_parameters.name);

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "measure",
        "strategy": {
            "static_estimated": {
                "list_size": 100,
                "max": 1.0,
                "subgraph": {
                    "all": {
                        "max": 1.0
                    }
                }
            }
        }
    });

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest::rstest]
#[case(basic_fragments(), "products")]
#[case(federated_ships_fragment(), "vehicles")]
async fn list_size_subgraph_inheritance_changes_estimates(
    #[case] test_parameters: TestSetupParameters,
    #[case] subgraph_name: &str,
    #[values(1, 10)] list_size: u64,
    #[values(None, Some(2))] all_list_size: Option<u64>,
    #[values(None, Some(3))] subgraph_list_size: Option<u64>,
) -> Result<(), BoxError> {
    // Tests various permutations of list_sizes (both specified and null) to ensure that those
    // list size defaults are being properly accounted for.
    set_snapshot_suffix!(
        "{}_{}_{}_{}",
        test_parameters.name,
        list_size,
        all_list_size.map_or_else(|| "null".to_string(), |s| s.to_string()),
        subgraph_list_size.map_or_else(|| "null".to_string(), |s| s.to_string())
    );

    let mut demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": list_size,
                "max": MAX_COST,
                "subgraph": {
                    "all": {},
                    "subgraphs": {
                        subgraph_name: {}
                    }
                }
            }
        }
    });

    if let Some(list_size) = all_list_size {
        let path = "/strategy/static_estimated/subgraph/all";
        *demand_control.pointer_mut(path).unwrap() = serde_json::json!({"list_size": list_size});
    }

    if let Some(list_size) = subgraph_list_size {
        let path = format!("/strategy/static_estimated/subgraph/subgraphs/{subgraph_name}");
        *demand_control.pointer_mut(&path).unwrap() = serde_json::json!({"list_size": list_size});
    }

    let response = query_supergraph_service(test_parameters, demand_control).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn coprocessor_can_access_and_mutate_costs() -> Result<(), BoxError> {
    let test_parameters = federated_ships_required();
    set_snapshot_suffix!("{}", test_parameters.name);

    // for this query and a configured list_size = 5:
    //   estimated_cost: 45.0
    //   estimated_cost_by_subgraph:
    //     users: 30.0
    //     vehicles: 15.0
    // the strategy is configured to reject the `users` queries (by setting subgraph.all.max = 20)
    // but the execution-stage request coprocessor will modify the estimated cost to allow the query to be fully
    // executed.
    // the actual values are then checked at the execution-stage response coprocessor.
    //
    // NB: I don't think real coprocessors _should_ mutate costs, but it's a useful way to test things.

    let mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/"))
        .respond_with(move |req: &wiremock::Request| {
            let request = req.body_json::<serde_json::Value>().expect("body");
            let stage = request.get("stage").and_then(|s| s.as_str()).unwrap_or("");

            let mut response = request.clone();
            match stage {
                "ExecutionRequest" => {
                    // read value of estimated_cost_by_subgraph for users, then set it to a different value.
                    let path =
                        "/context/entries/apollo::demand_control::estimated_cost_by_subgraph/users";

                    let users_cost = response.pointer_mut(path).unwrap();
                    assert_eq!(users_cost.as_f64(), Some(30.0));
                    *users_cost = 15.0.into();
                }
                "ExecutionResponse" => {
                    // should see actual costs and results for both subgraphs
                    let path = "/context/entries/apollo::demand_control::actual_cost_by_subgraph";
                    let actual_costs = request.pointer(path).unwrap();
                    assert_eq!(actual_costs["users"].as_f64(), Some(6.0));
                    assert_eq!(actual_costs["vehicles"].as_f64(), Some(9.0));

                    let path = "/context/entries/apollo::demand_control::result_by_subgraph";
                    let results = request.pointer(path).unwrap();
                    assert_eq!(results["users"].as_str(), Some("COST_OK"));
                    assert_eq!(results["vehicles"].as_str(), Some("COST_OK"));
                }
                _ => panic!("unexpected stage `{stage}`"),
            }
            ResponseTemplate::new(200).set_body_json(response)
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    let demand_control = serde_json::json!({
        "enabled": true,
        "mode": "enforce",
        "strategy": {
            "static_estimated": {
                "list_size": 5,
                "max": MAX_COST,
                "subgraph": {
                    "all": {
                        "max": 20.0
                    }
                }
            }
        }
    });

    let coprocessor = serde_json::json!({
        "url": mock_server.uri(),
        "execution": {
            "request": {
                "context": {
                    "selective": ["apollo::demand_control::estimated_cost_by_subgraph"],
                },
            },
            "response": {
                "context": {
                    "selective": [
                        "apollo::demand_control::actual_cost_by_subgraph",
                        "apollo::demand_control::result_by_subgraph"
                    ],
                },
            },
        },
    });

    let service = TestHarness::builder()
        .schema(test_parameters.schema)
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": {"all": true},
            "coprocessor": coprocessor,
            "demand_control": demand_control,
            "experimental_mock_subgraphs": test_parameters.subgraphs,
        }))?
        .build_supergraph()
        .await?;

    let request = supergraph::Request::fake_builder()
        .query(test_parameters.query)
        .build()?;
    let response = service.oneshot(request).await?;
    insta::assert_json_snapshot!(parse_result_for_snapshot(response).await);
    Ok(())
}
