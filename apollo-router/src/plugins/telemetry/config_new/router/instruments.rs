use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::ActiveRequestsAttributes;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::RouterValue;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterInstrumentsConfig {
    /// Histogram of server request duration
    #[serde(rename = "http.server.request.duration")]
    pub(crate) http_server_request_duration:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Counter of active requests
    #[serde(rename = "http.server.active_requests")]
    pub(crate) http_server_active_requests: DefaultedStandardInstrument<ActiveRequestsAttributes>,

    /// Histogram of server request body size
    #[serde(rename = "http.server.request.body.size")]
    pub(crate) http_server_request_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of server response body size
    #[serde(rename = "http.server.response.body.size")]
    pub(crate) http_server_response_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,
}

impl DefaultForLevel for RouterInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.http_server_request_duration
            .defaults_for_levels(requirement_level, kind);
        self.http_server_active_requests
            .defaults_for_levels(requirement_level, kind);
        self.http_server_request_body_size
            .defaults_for_levels(requirement_level, kind);
        self.http_server_response_body_size
            .defaults_for_levels(requirement_level, kind);
    }
}

pub(crate) struct RouterInstruments {
    pub(crate) http_server_request_duration: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) http_server_active_requests:
        Option<crate::plugins::telemetry::config_new::instruments::ActiveRequestsCounter>,
    pub(crate) http_server_request_body_size: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) http_server_response_body_size: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) custom: RouterCustomInstruments,
}

impl Instrumented for RouterInstruments {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_request(request);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_request(request);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_request(request);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_response(response);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_response(response);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_response(response);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_error(error, ctx);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_error(error, ctx);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_error(error, ctx);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }
}

pub(crate) type RouterCustomInstruments = CustomInstruments<
    router::Request,
    router::Response,
    (),
    RouterAttributes,
    RouterSelector,
    RouterValue,
>;

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use http::StatusCode;
    use serde_json::json;
    use tower::BoxError;

    use crate::Context;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::config_new::instruments::Instrumented;
    use crate::plugins::telemetry::config_new::instruments::InstrumentsConfig;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;

    #[tokio::test]
    async fn test_router_instruments() {
        // Please don't add further logic to this test, it's already testing multiple things.
        // Instead, add a data-driven test via test_instruments test.
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "router": {
                        "http.server.request.body.size": true,
                        "http.server.response.body.size": {
                            "attributes": {
                                "http.response.status_code": false,
                                "acme.my_attribute": {
                                    "response_header": "x-my-header",
                                    "default": "unknown"
                                }
                            }
                        },
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
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.on_critical_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "request time out",
                                    {
                                        "error": "reason"
                                    }
                                ]
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.on_error_histo": {
                            "value": "unit",
                            "type": "histogram",
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
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.header_value": {
                            "value": {
                                "request_header": "x-my-header-count"
                            },
                            "type": "counter",
                            "description": "my description",
                            "unit": "nb"
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("conditional-custom", "X")
                .header("x-my-header-count", "55")
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("x-my-header", "TEST")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 55.0);
            assert_counter!(
                "acme.request.on_error",
                1.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                1.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!("http.server.request.body.size", 35.0);
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "TEST"
            );

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("x-my-header-count", "5")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 60.0);
            assert_counter!(
                "acme.request.on_error",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!("http.server.request.body.size", 70.0);
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "TEST"
            );
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "unknown"
            );

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 60.0);
            assert_counter!(
                "acme.request.on_error",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                2.0,
                "http.response.status_code" = 400
            );

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            router_instruments.on_error(&BoxError::from("request time out"), &Context::new());
            assert_counter!(
                "acme.request.on_critical_error",
                1.0,
                "http.response.status_code" = 500
            );
        }
        .with_metrics()
        .await;
    }
}
