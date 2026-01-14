use apollo_router::Context;
use apollo_router::graphql;

const CODE_OK: &str = "COST_OK";
const CODE_TOO_EXPENSIVE: &str = "COST_ESTIMATED_TOO_EXPENSIVE";
const CODE_SUBGRAPH_TOO_EXPENSIVE: &str = "SUBGRAPH_COST_ESTIMATED_TOO_EXPENSIVE";

fn get_strategy(context: &Context) -> String {
    let field = "apollo::demand_control::strategy";
    context
        .get::<_, String>(field)
        .expect("can't deserialize")
        .expect(&format!("context missing {field}"))
}

fn get_result(context: &Context) -> String {
    let field = "apollo::demand_control::result";
    context
        .get::<_, String>(field)
        .expect("can't deserialize")
        .expect(&format!("context missing {field}"))
}

fn get_result_by_subgraph(context: &Context) -> Option<serde_json::Value> {
    context
        .get::<_, serde_json::Value>("apollo::demand_control::result_by_subgraph")
        .expect("can't deserialize")
}

fn get_actual_cost(context: &Context) -> Option<f64> {
    context
        .get::<_, f64>("apollo::demand_control::actual_cost")
        .expect("can't deserialize")
}

fn get_estimated_cost(context: &Context) -> Option<f64> {
    context
        .get::<_, f64>("apollo::demand_control::estimated_cost")
        .expect("can't deserialize")
}

fn get_estimated_cost_by_subgraph(context: &Context) -> Option<serde_json::Value> {
    context
        .get::<_, serde_json::Value>("apollo::demand_control::estimated_cost_by_subgraph")
        .expect("can't deserialize")
}

fn get_subgraph_call_count(context: &Context) -> Option<serde_json::Value> {
    context
        .get::<_, serde_json::Value>("apollo::experimental_mock_subgraphs::subgraph_call_count")
        .expect("can't deserialize")
}

fn estimated_too_expensive(error: &&graphql::Error) -> bool {
    error
        .extensions
        .get("code")
        .map_or(false, |code| code == CODE_TOO_EXPENSIVE)
}

fn subgraph_estimated_too_expensive(error: &&graphql::Error) -> bool {
    error
        .extensions
        .get("code")
        .map_or(false, |code| code == CODE_SUBGRAPH_TOO_EXPENSIVE)
}

mod basic_fragments_tests {
    use apollo_router::TestHarness;
    use apollo_router::services::supergraph;
    use tokio_stream::StreamExt;
    use tower::BoxError;
    use tower::ServiceExt;

    use super::CODE_OK;
    use super::CODE_SUBGRAPH_TOO_EXPENSIVE;
    use super::CODE_TOO_EXPENSIVE;
    use super::estimated_too_expensive;
    use super::get_estimated_cost;
    use super::get_estimated_cost_by_subgraph;
    use super::get_result;
    use super::get_result_by_subgraph;
    use super::get_strategy;
    use super::get_subgraph_call_count;
    use super::subgraph_estimated_too_expensive;

    fn schema() -> &'static str {
        include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/basic_supergraph_schema.graphql"
        )
    }

    fn query() -> &'static str {
        include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/basic_fragments_query.graphql"
        )
    }

    fn subgraphs() -> serde_json::Value {
        serde_json::json!({
            "products": {
                "query": {
                    "interfaceInstance1": {"__typename": "SecondObjectType", "field1": null, "field2": "hello"},
                    "someUnion": {"__typename": "FirstObjectType", "innerList": []}
                },
            }
        })
    }

    async fn supergraph_service(
        demand_control: serde_json::Value,
    ) -> Result<supergraph::BoxCloneService, BoxError> {
        TestHarness::builder()
            .schema(schema())
            .configuration_json(serde_json::json!({
                "include_subgraph_errors": {"all": true},
                "demand_control": demand_control,
                "experimental_mock_subgraphs": subgraphs(),
            }))?
            .build_supergraph()
            .await
    }

    async fn query_supergraph_service(
        demand_control: serde_json::Value,
    ) -> Result<supergraph::Response, BoxError> {
        let service = supergraph_service(demand_control).await?;
        let request = supergraph::Request::fake_builder().query(query()).build()?;
        service.oneshot(request).await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[rstest::rstest]
    async fn requests_within_max_are_accepted(
        #[values(12.0, 15.0)] max_cost: f64,
    ) -> Result<(), BoxError> {
        // query total cost is 12.0; max_cost >= 12.0 should result in query being accepted
        let demand_control = serde_json::json!({
            "enabled": true,
            "mode": "enforce",
            "strategy": {
                "static_estimated_by_subgraph": {
                    "list_size": 10,
                    "max": max_cost
                }
            }
        });

        let response = query_supergraph_service(demand_control).await?;

        let context = response.context;
        let body = response.response.into_body().next().await.unwrap();

        // estimates
        assert_eq!(&get_strategy(&context), "static_estimated_by_subgraph");
        assert_eq!(&get_result(&context), CODE_OK);
        assert_eq!(get_estimated_cost(&context).unwrap(), 12.0);

        assert_eq!(
            get_result_by_subgraph(&context).unwrap(),
            serde_json::json!({ "products": CODE_OK })
        );
        assert_eq!(
            get_estimated_cost_by_subgraph(&context).unwrap(),
            serde_json::json!({ "products": 12.0 })
        );

        // actuals
        assert!(body.data.is_some());
        assert!(body.errors.is_empty());

        let subgraph_call_count = get_subgraph_call_count(&context).unwrap();
        assert_eq!(subgraph_call_count["products"], 1);

        // TODO: check actuals, once we figure out how to handle by-subgraph actuals

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    #[rstest::rstest]
    async fn requests_exceeding_max_are_rejected(
        #[values(5.0, 10.0)] max_cost: f64,
    ) -> Result<(), BoxError> {
        // query total cost is 12.0; all `max_cost` values are less than this, so the response should
        // be an error
        let demand_control = serde_json::json!({
            "enabled": true,
            "mode": "enforce",
            "strategy": {
                "static_estimated_by_subgraph": {
                    "list_size": 10,
                    "max": max_cost
                }
            }
        });

        let response = query_supergraph_service(demand_control).await?;

        let context = response.context;
        let body = response.response.into_body().next().await.unwrap();

        // estimates
        assert_eq!(&get_strategy(&context), "static_estimated_by_subgraph");
        assert_eq!(&get_result(&context), CODE_TOO_EXPENSIVE);
        assert_eq!(get_estimated_cost(&context).unwrap(), 12.0);

        assert_eq!(
            get_estimated_cost_by_subgraph(&context).unwrap(),
            serde_json::json!({ "products": 12.0 })
        );

        // actuals
        assert!(body.data.is_none());

        let error = body.errors.iter().find(estimated_too_expensive).unwrap();
        assert_eq!(error.extensions["cost.estimated"], 12.0);
        assert_eq!(error.extensions["cost.max"], max_cost);

        assert!(get_subgraph_call_count(&context).is_none());

        Ok(())
    }

    #[tokio::test]
    async fn requests_which_exceed_subgraph_limit_are_partially_accepted() -> Result<(), BoxError> {
        // query checks products once; query should be accepted based on max but products subgraph
        // should not be called.
        let demand_control = serde_json::json!({
            "enabled": true,
            "mode": "enforce",
            "strategy": {
                "static_estimated_by_subgraph": {
                    "list_size": 10,
                    "max": 15,
                    "subgraphs": {
                        "all": {},
                        "subgraphs": {
                            "products": {
                                "max": 10
                            }
                        }
                    }
                }
            }
        });

        let response = query_supergraph_service(demand_control).await?;

        let context = response.context;
        let body = response.response.into_body().next().await.unwrap();

        // estimates
        assert_eq!(&get_strategy(&context), "static_estimated_by_subgraph");
        assert_eq!(&get_result(&context), CODE_OK);
        assert_eq!(get_estimated_cost(&context).unwrap(), 12.0);
        assert_eq!(
            get_result_by_subgraph(&context).unwrap(),
            serde_json::json!({ "products": CODE_SUBGRAPH_TOO_EXPENSIVE })
        );
        assert_eq!(
            get_estimated_cost_by_subgraph(&context).unwrap(),
            serde_json::json!({ "products": 12.0})
        );

        // actuals
        assert!(body.data.is_some());
        assert!(body.errors.iter().find(estimated_too_expensive).is_none());

        let error = body
            .errors
            .iter()
            .find(subgraph_estimated_too_expensive)
            .unwrap();
        assert_eq!(error.extensions["cost.subgraph.estimated"], 12.0);
        assert_eq!(error.extensions["cost.subgraph.max"], 10.0);

        let subgraph_call_count = get_subgraph_call_count(&context).unwrap_or_default();
        assert!(subgraph_call_count.get("products").is_none());

        // TODO: check actuals, once we figure out how to handle by-subgraph actuals

        Ok(())
    }
}

mod federated_ships_tests {
    use apollo_router::TestHarness;
    use apollo_router::services::supergraph;
    use tokio_stream::StreamExt;
    use tower::BoxError;
    use tower::ServiceExt;

    use super::CODE_OK;
    use super::CODE_SUBGRAPH_TOO_EXPENSIVE;
    use super::CODE_TOO_EXPENSIVE;
    use super::estimated_too_expensive;
    use super::get_estimated_cost;
    use super::get_estimated_cost_by_subgraph;
    use super::get_result;
    use super::get_result_by_subgraph;
    use super::get_strategy;
    use super::get_subgraph_call_count;
    use super::subgraph_estimated_too_expensive;

    fn schema() -> &'static str {
        include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        )
    }

    fn query() -> &'static str {
        include_str!(
            "../../src/plugins/demand_control/cost_calculator/fixtures/federated_ships_required_query.graphql"
        )
    }

    fn subgraphs() -> serde_json::Value {
        serde_json::json!({
            "vehicles": {
                "query": {
                    "ships": [
                        {"__typename": "Ship", "id": 1, "name": "Ship1", "owner": {"__typename": "User", "licenseNumber": 10},},
                        {"__typename": "Ship", "id": 2, "name": "Ship2", "owner": {"__typename": "User", "licenseNumber": 11},},
                        {"__typename": "Ship", "id": 3, "name": "Ship3", "owner": {"__typename": "User", "licenseNumber": 12},},
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
        })
    }

    async fn supergraph_service(
        demand_control: serde_json::Value,
    ) -> Result<supergraph::BoxCloneService, BoxError> {
        TestHarness::builder()
            .schema(schema())
            .configuration_json(serde_json::json!({
                "include_subgraph_errors": {"all": true},
                "demand_control": demand_control,
                "experimental_mock_subgraphs": subgraphs(),
            }))?
            .build_supergraph()
            .await
    }

    async fn query_supergraph_service(
        demand_control: serde_json::Value,
    ) -> Result<supergraph::Response, BoxError> {
        let service = supergraph_service(demand_control).await?;
        let request = supergraph::Request::fake_builder().query(query()).build()?;
        service.oneshot(request).await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[rstest::rstest]
    async fn requests_within_max_are_accepted(
        #[values(10400.0, 10500.0)] max_cost: f64,
    ) -> Result<(), BoxError> {
        // query total cost is 10400 for list_size = 100; all `max_cost` values are geq than this,
        // so the response should be OK
        let demand_control = serde_json::json!({
            "enabled": true,
            "mode": "enforce",
            "strategy": {
                "static_estimated_by_subgraph": {
                    "list_size": 100,
                    "max": max_cost
                }
            }
        });

        let response = query_supergraph_service(demand_control).await?;

        let context = response.context;
        let body = response.response.into_body().next().await.unwrap();

        // estimates
        assert_eq!(&get_strategy(&context), "static_estimated_by_subgraph");
        assert_eq!(&get_result(&context), CODE_OK);
        assert_eq!(get_estimated_cost(&context).unwrap(), 10400.0);

        assert_eq!(
            get_result_by_subgraph(&context).unwrap(),
            serde_json::json!({ "users": CODE_OK, "vehicles": CODE_OK })
        );
        assert_eq!(
            get_estimated_cost_by_subgraph(&context).unwrap(),
            serde_json::json!({ "users": 10100.0, "vehicles": 300.0 })
        );

        // actuals
        assert!(body.data.is_some());
        assert!(body.errors.is_empty());

        let subgraph_call_count = get_subgraph_call_count(&context).unwrap();
        assert_eq!(subgraph_call_count["users"], 1);
        assert_eq!(subgraph_call_count["vehicles"], 2);

        // TODO: check actuals, once we figure out how to handle by-subgraph actuals

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    #[rstest::rstest]
    async fn requests_exceeding_max_are_rejected(
        #[values(100.0, 10000.0)] max_cost: f64,
    ) -> Result<(), BoxError> {
        // query total cost is 10400 for list_size = 100; all `max_cost` values are less than this,
        // so the response should be an error
        let demand_control = serde_json::json!({
            "enabled": true,
            "mode": "enforce",
            "strategy": {
                "static_estimated_by_subgraph": {
                    "list_size": 100,
                    "max": max_cost
                }
            }
        });

        let response = query_supergraph_service(demand_control).await?;

        let context = response.context;
        let body = response.response.into_body().next().await.unwrap();

        // estimates
        assert_eq!(&get_strategy(&context), "static_estimated_by_subgraph");
        assert_eq!(&get_result(&context), CODE_TOO_EXPENSIVE);
        assert_eq!(get_estimated_cost(&context).unwrap(), 10400.0);

        assert_eq!(
            get_estimated_cost_by_subgraph(&context).unwrap(),
            serde_json::json!({ "users": 10100.0, "vehicles": 300.0 })
        );

        // actuals
        assert!(body.data.is_none());

        let error = body.errors.iter().find(estimated_too_expensive).unwrap();
        assert_eq!(error.extensions["cost.estimated"], 10400.0);
        assert_eq!(error.extensions["cost.max"], max_cost);

        assert!(get_subgraph_call_count(&context).is_none());

        Ok(())
    }

    #[tokio::test]
    async fn requests_which_exceed_subgraph_limit_are_partially_accepted() -> Result<(), BoxError> {
        // query checks vehicles, then users, then vehicles.
        // interrupting the users check via a demand control limit should still permit both vehicles
        // checks.
        let demand_control = serde_json::json!({
            "enabled": true,
            "mode": "enforce",
            "strategy": {
                "static_estimated_by_subgraph": {
                    "list_size": 100,
                    "max": 15000.0,
                    "subgraphs": {
                        "all": {},
                        "subgraphs": {
                            "users": {
                                "max": 0
                            }
                        }
                    }
                }
            }
        });

        let response = query_supergraph_service(demand_control).await?;

        let context = response.context;
        let body = response.response.into_body().next().await.unwrap();

        // estimates
        assert_eq!(&get_strategy(&context), "static_estimated_by_subgraph");
        assert_eq!(&get_result(&context), CODE_OK);
        assert_eq!(get_estimated_cost(&context).unwrap(), 10400.0);
        assert_eq!(
            get_result_by_subgraph(&context).unwrap(),
            serde_json::json!({ "users": CODE_SUBGRAPH_TOO_EXPENSIVE, "vehicles": CODE_OK })
        );
        assert_eq!(
            get_estimated_cost_by_subgraph(&context).unwrap(),
            serde_json::json!({ "users": 10100.0, "vehicles": 300.0 })
        );

        // actuals
        assert!(body.data.is_some());
        assert!(body.errors.iter().find(estimated_too_expensive).is_none());

        let error = body
            .errors
            .iter()
            .find(subgraph_estimated_too_expensive)
            .unwrap();
        assert_eq!(error.extensions["cost.subgraph.estimated"], 10100.0);
        assert_eq!(error.extensions["cost.subgraph.max"], 0.0);

        let subgraph_call_count = get_subgraph_call_count(&context).unwrap();
        assert!(subgraph_call_count.get("users").is_none());
        assert_eq!(subgraph_call_count["vehicles"], 2);

        // TODO: check actuals, once we figure out how to handle by-subgraph actuals

        Ok(())
    }
}
