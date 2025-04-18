use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::cost::CostInstruments;
use crate::plugins::telemetry::config_new::cost::CostInstrumentsConfig;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphValue;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphInstrumentsConfig {
    #[serde(flatten)]
    pub(crate) cost: CostInstrumentsConfig,
}

impl DefaultForLevel for SupergraphInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
    }
}

pub(crate) struct SupergraphInstruments {
    pub(crate) cost: CostInstruments,
    pub(crate) custom: SupergraphCustomInstruments,
}

impl Instrumented for SupergraphInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        self.cost.on_request(request);
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        self.cost.on_response(response);
        self.custom.on_response(response);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        self.cost.on_response_event(response, ctx);
        self.custom.on_response_event(response, ctx);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        self.cost.on_error(error, ctx);
        self.custom.on_error(error, ctx);
    }
}

pub(crate) type SupergraphCustomInstruments = CustomInstruments<
    supergraph::Request,
    supergraph::Response,
    crate::graphql::Response,
    SupergraphAttributes,
    SupergraphSelector,
    SupergraphValue,
>;

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use http::StatusCode;
    use serde_json::json;

    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_KIND;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::config_new::instruments::Instrumented;
    use crate::plugins::telemetry::config_new::instruments::InstrumentsConfig;
    use crate::plugins::telemetry::config_new::supergraph::instruments::SupergraphCustomInstruments;
    use crate::services::supergraph;

    #[tokio::test]
    async fn test_supergraph_instruments() {
        // Please don't add further logic to this test, it's already testing multiple things.
        // Instead, add a data\driven test via test_instruments test.
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "supergraph": {
                        "acme.request.on_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            }
                        },
                        "acme.request.on_graphql_error": {
                            "value": "event_unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "NOPE",
                                    {
                                        "response_errors": "$.[0].extensions.code"
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_error_selector": {
                            "value": "event_unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    true,
                                    {
                                        "on_graphql_error": true
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_error_histo": {
                            "value": "event_unit",
                            "type": "histogram",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "NOPE",
                                    {
                                        "response_errors": "$.[0].extensions.code"
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_data": {
                            "value": {
                                "response_data": "$.price"
                            },
                            "type": "counter",
                            "unit": "$",
                            "description": "my description",
                            "attributes": {
                                "response.data": {
                                    "response_data": "$.*"
                                }
                            }
                        },
                        "acme.query": {
                            "value": "unit",
                            "type": "counter",
                            "description": "nb of queries",
                            "condition": {
                                "eq": [
                                    "query",
                                    {
                                        "operation_kind": "string"
                                    }
                                ]
                            },
                            "unit": "query",
                            "attributes": {
                                "query": {
                                    "query": "string"
                                }
                            }
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let custom_instruments = SupergraphCustomInstruments::new(
                &config.supergraph.custom,
                Arc::new(config.new_builtin_supergraph_instruments()),
            );
            let context = crate::context::Context::new();
            let _ = context.insert(OPERATION_KIND, "query".to_string()).unwrap();
            let context_with_error = crate::context::Context::new();
            let _ = context_with_error
                .insert(OPERATION_KIND, "query".to_string())
                .unwrap();
            let _ = context_with_error
                .insert(CONTAINS_GRAPHQL_ERROR, true)
                .unwrap();
            let supergraph_req = supergraph::Request::fake_builder()
                .header("conditional-custom", "X")
                .header("x-my-header-count", "55")
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .query("{me{name}}")
                .context(context.clone())
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("x-my-header", "TEST")
                .header("content-length", "35")
                .errors(vec![
                    graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build(),
                ])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("nope")
                            .extension_code("NOPE")
                            .build(),
                    ])
                    .build(),
                &context_with_error,
            );

            assert_counter!("acme.query", 1.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 1.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 500.0, response.data = 500);

            let custom_instruments = SupergraphCustomInstruments::new(
                &config.supergraph.custom,
                Arc::new(config.new_builtin_supergraph_instruments()),
            );
            let supergraph_req = supergraph::Request::fake_builder()
                .header("content-length", "35")
                .header("x-my-header-count", "5")
                .header("content-type", "application/graphql")
                .context(context.clone())
                .query("Subscription {me{name}}")
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .errors(vec![
                    graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build(),
                ])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("nope")
                            .extension_code("NOPE")
                            .build(),
                    ])
                    .build(),
                &context_with_error,
            );

            assert_counter!("acme.query", 1.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 2.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 1000.0, response.data = 500);

            let custom_instruments = SupergraphCustomInstruments::new(
                &config.supergraph.custom,
                Arc::new(config.new_builtin_supergraph_instruments()),
            );
            let supergraph_req = supergraph::Request::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .context(context.clone())
                .query("{me{name}}")
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(serde_json_bytes::json!({"foo": "bar"}))
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(serde_json_bytes::json!({"foo": "bar"}))
                    .build(),
                &supergraph_req.context,
            );

            assert_counter!("acme.query", 2.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 2.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 1000.0, response.data = 500);
        }
        .with_metrics()
        .await;
    }
}
