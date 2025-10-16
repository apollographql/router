use derivative::Derivative;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::from_str;
use serde_json_bytes::path::JsonPathInst;
use sha2::Digest;

use super::events::DisplayRouterResponse;
use crate::Context;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::OPERATION_NAME;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::TraceIdFormat;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;
use crate::plugins::telemetry::config_new::ToOtelValue;
use crate::plugins::telemetry::config_new::get_baggage;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::plugins::telemetry::config_new::router::events::RouterResponseBodyExtensionType;
use crate::plugins::telemetry::config_new::selectors::ErrorRepr;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
use crate::plugins::telemetry::config_new::trace_id;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::router;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum RouterValue {
    Standard(Standard),
    Custom(RouterSelector),
}

impl From<&RouterValue> for InstrumentValue<RouterSelector> {
    fn from(value: &RouterValue) -> Self {
        match value {
            RouterValue::Standard(standard) => InstrumentValue::Standard(standard.clone()),
            RouterValue::Custom(selector) => InstrumentValue::Custom(selector.clone()),
        }
    }
}

#[derive(Derivative, Deserialize, JsonSchema, Clone)]
#[serde(deny_unknown_fields, untagged)]
#[derivative(Debug, PartialEq)]
pub(crate) enum RouterSelector {
    /// A value from baggage.
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from an environment variable.
    Env {
        /// The name of the environment variable
        env: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
        /// Avoid unsafe std::env::set_var in tests
        #[cfg(test)]
        #[serde(skip)]
        mocked_env_var: Option<String>,
    },
    /// Critical error if it happens
    Error {
        #[allow(dead_code)]
        error: ErrorRepr,
    },
    /// Boolean set to true if the response body contains graphql error
    OnGraphQLError { on_graphql_error: bool },
    /// The operation name from the query.
    OperationName {
        /// The operation name from the query.
        operation_name: OperationName,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    /// A header from the request
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from context.
    RequestContext {
        /// The request context key.
        request_context: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// The request method.
    RequestMethod {
        /// The request method enabled or not
        request_method: bool,
    },
    /// The body of the response
    ResponseBody {
        /// The response body enabled or not
        response_body: bool,
    },
    /// The body response errors
    ResponseErrors {
        /// The router response body json path of the chunks.
        #[schemars(with = "String")]
        #[derivative(Debug = "ignore", PartialEq = "ignore")]
        #[serde(deserialize_with = "deserialize_jsonpath")]
        response_errors: JsonPathInst,
    },
    /// A header from the response
    ResponseHeader {
        /// The name of the request header.
        response_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from context.
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A status from the response
    ResponseStatus {
        /// The http response status code.
        response_status: ResponseStatus,
    },
    /// Deprecated, should not be used anymore, use static field instead
    Static(String),
    StaticField {
        /// A static value
        r#static: AttributeValue,
    },
    /// Apollo Studio operation id
    StudioOperationId {
        /// Apollo Studio operation id
        studio_operation_id: bool,
    },
    /// The trace ID of the request.
    TraceId {
        /// The format of the trace ID.
        trace_id: TraceIdFormat,
    },
}

impl Selector for RouterSelector {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &router::Request) -> Option<opentelemetry::Value> {
        // Helper function to insert DisplayRouterResponse into request context extensions
        fn insert_display_router_response(request: &router::Request) {
            request.context.extensions().with_lock(|ext| {
                ext.insert(DisplayRouterResponse);
            });
        }

        match self {
            RouterSelector::RequestMethod { request_method } if *request_method => {
                Some(request.router_request.method().to_string().into())
            }
            RouterSelector::RequestContext {
                request_context,
                default,
                ..
            } => request
                .context
                .get_json_value(request_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            RouterSelector::RequestHeader {
                request_header,
                default,
                ..
            } => request
                .router_request
                .headers()
                .get(request_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string().into()))
                .or_else(|| default.maybe_to_otel_value()),
            RouterSelector::Env {
                env,
                default,
                #[cfg(test)]
                mocked_env_var,
                ..
            } => {
                #[cfg(test)]
                let value = mocked_env_var.clone();
                #[cfg(not(test))]
                let value = None;
                value
                    .or_else(|| std::env::var(env).ok())
                    .or_else(|| default.clone())
                    .map(opentelemetry::Value::from)
            }
            RouterSelector::TraceId {
                trace_id: trace_id_format,
            } => trace_id().map(|id| trace_id_format.format(id).into()),
            RouterSelector::Baggage {
                baggage, default, ..
            } => get_baggage(baggage).or_else(|| default.maybe_to_otel_value()),
            RouterSelector::Static(val) => Some(val.clone().into()),
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            RouterSelector::ResponseBody { response_body } if *response_body => {
                insert_display_router_response(request);
                None
            }
            RouterSelector::ResponseErrors { .. } => {
                insert_display_router_response(request);
                None
            }
            // Related to Response
            _ => None,
        }
    }

    fn on_response(&self, response: &router::Response) -> Option<opentelemetry::Value> {
        match self {
            RouterSelector::ResponseBody { response_body } if *response_body => {
                response
                    .context
                    .extensions()
                    .with_lock(|ext| {
                        // Clone here in case anything else also needs access to the body
                        ext.get::<RouterResponseBodyExtensionType>().cloned()
                    })
                    .map(|v| opentelemetry::Value::String(v.0.into()))
            }
            RouterSelector::ResponseErrors { response_errors } => response
                .context
                .extensions()
                .with_lock(|ext| ext.get::<RouterResponseBodyExtensionType>().cloned())
                .and_then(|v| {
                    from_str::<serde_json::Value>(&v.0)
                        .ok()
                        .and_then(|body_json| {
                            let errors = body_json.get("errors");

                            let data: serde_json_bytes::Value =
                                serde_json_bytes::to_value(errors).ok()?;

                            let val = response_errors.find(&data);

                            val.maybe_to_otel_value()
                        })
                }),
            RouterSelector::ResponseHeader {
                response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(response_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string().into()))
                .or_else(|| default.maybe_to_otel_value()),
            RouterSelector::ResponseStatus { response_status } => match response_status {
                ResponseStatus::Code => Some(opentelemetry::Value::I64(
                    response.response.status().as_u16() as i64,
                )),
                ResponseStatus::Reason => response
                    .response
                    .status()
                    .canonical_reason()
                    .map(|reason| reason.to_string().into()),
            },
            RouterSelector::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            RouterSelector::OperationName {
                operation_name,
                default,
                ..
            } => {
                let op_name = response.context.get(OPERATION_NAME).ok().flatten();
                match operation_name {
                    OperationName::String => op_name.or_else(|| default.clone()),
                    OperationName::Hash => op_name.or_else(|| default.clone()).map(|op_name| {
                        let mut hasher = sha2::Sha256::new();
                        hasher.update(op_name.as_bytes());
                        let result = hasher.finalize();
                        hex::encode(result)
                    }),
                }
                .map(opentelemetry::Value::from)
            }
            RouterSelector::Baggage {
                baggage, default, ..
            } => get_baggage(baggage).or_else(|| default.maybe_to_otel_value()),
            RouterSelector::OnGraphQLError { on_graphql_error } if *on_graphql_error => {
                let contains_error = response
                    .context
                    .get_json_value(CONTAINS_GRAPHQL_ERROR)
                    .and_then(|value| value.as_bool())
                    .unwrap_or_default();
                Some(opentelemetry::Value::Bool(contains_error))
            }
            RouterSelector::Static(val) => Some(val.clone().into()),
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            RouterSelector::StudioOperationId {
                studio_operation_id,
            } if *studio_operation_id => response
                .context
                .get::<_, String>(APOLLO_OPERATION_ID)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            _ => None,
        }
    }

    fn on_error(&self, error: &tower::BoxError, ctx: &Context) -> Option<opentelemetry::Value> {
        match self {
            RouterSelector::Error { .. } => Some(error.to_string().into()),
            RouterSelector::Static(val) => Some(val.clone().into()),
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            RouterSelector::ResponseContext {
                response_context,
                default,
                ..
            } => ctx
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            RouterSelector::OperationName {
                operation_name,
                default,
                ..
            } => {
                let op_name = ctx.get(OPERATION_NAME).ok().flatten();
                match operation_name {
                    OperationName::String => op_name.or_else(|| default.clone()),
                    OperationName::Hash => op_name.or_else(|| default.clone()).map(|op_name| {
                        let mut hasher = sha2::Sha256::new();
                        hasher.update(op_name.as_bytes());
                        let result = hasher.finalize();
                        hex::encode(result)
                    }),
                }
                .map(opentelemetry::Value::from)
            }
            _ => None,
        }
    }

    fn on_drop(&self) -> Option<opentelemetry::Value> {
        match self {
            RouterSelector::Static(val) => Some(val.clone().into()),
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn is_active(&self, stage: Stage) -> bool {
        match stage {
            Stage::Request => {
                matches!(
                    self,
                    RouterSelector::RequestHeader { .. }
                        | RouterSelector::RequestContext { .. }
                        | RouterSelector::RequestMethod { .. }
                        | RouterSelector::TraceId { .. }
                        | RouterSelector::StudioOperationId { .. }
                        | RouterSelector::Baggage { .. }
                        | RouterSelector::Static(_)
                        | RouterSelector::Env { .. }
                        | RouterSelector::StaticField { .. }
                )
            }
            Stage::Response | Stage::ResponseEvent => matches!(
                self,
                RouterSelector::TraceId { .. }
                    | RouterSelector::StudioOperationId { .. }
                    | RouterSelector::OperationName { .. }
                    | RouterSelector::Baggage { .. }
                    | RouterSelector::Static(_)
                    | RouterSelector::Env { .. }
                    | RouterSelector::StaticField { .. }
                    | RouterSelector::ResponseHeader { .. }
                    | RouterSelector::ResponseContext { .. }
                    | RouterSelector::ResponseStatus { .. }
                    | RouterSelector::OnGraphQLError { .. }
            ),
            Stage::ResponseField => false,
            Stage::Error => matches!(
                self,
                RouterSelector::TraceId { .. }
                    | RouterSelector::StudioOperationId { .. }
                    | RouterSelector::OperationName { .. }
                    | RouterSelector::Baggage { .. }
                    | RouterSelector::Static(_)
                    | RouterSelector::Env { .. }
                    | RouterSelector::StaticField { .. }
                    | RouterSelector::ResponseContext { .. }
                    | RouterSelector::Error { .. }
            ),
            Stage::Drop => matches!(
                self,
                RouterSelector::Static(_) | RouterSelector::StaticField { .. }
            ),
        }
    }
}

#[cfg(test)]
mod test {
    use http::StatusCode;
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry::baggage::BaggageExt;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use serde_json::json;
    use serde_json_bytes::path::JsonPathInst;
    use tower::BoxError;
    use tracing::span;
    use tracing::subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::context::OPERATION_NAME;
    use crate::plugins::telemetry::TraceIdFormat;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::selectors::OperationName;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::otel;
    use crate::query_planner::APOLLO_OPERATION_ID;

    #[test]
    fn router_static() {
        let selector = RouterSelector::Static("test_static".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "test_static".into()
        );
        assert_eq!(selector.on_drop().unwrap(), "test_static".into());
    }

    #[test]
    fn router_static_field() {
        let selector = RouterSelector::StaticField {
            r#static: "test_static".to_string().into(),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "test_static".into()
        );
        assert_eq!(selector.on_drop().unwrap(), "test_static".into());
    }

    #[test]
    fn router_request_header() {
        let selector = RouterSelector::RequestHeader {
            request_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .header("header_key", "header_value")
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "header_value".into()
        );

        assert_eq!(
            selector
                .on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_response(
                &crate::services::RouterResponse::fake_builder()
                    .context(crate::context::Context::default())
                    .header("header_key", "header_value")
                    .data(json!({}))
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn router_request_context() {
        let selector = RouterSelector::RequestContext {
            request_context: "context_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        let _ = context.insert("context_key".to_string(), "context_value".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .context(context.clone())
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "context_value".into()
        );

        assert_eq!(
            selector
                .on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );
        assert_eq!(
            selector.on_response(
                &crate::services::RouterResponse::fake_builder()
                    .context(context)
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn router_response_context() {
        let selector = RouterSelector::ResponseContext {
            response_context: "context_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        let _ = context.insert("context_key".to_string(), "context_value".to_string());
        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .context(context.clone())
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "context_value".into()
        );

        assert_eq!(
            selector
                .on_error(&BoxError::from(String::from("my error")), &context)
                .unwrap(),
            "context_value".into()
        );

        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );
        assert_eq!(
            selector.on_request(
                &crate::services::RouterRequest::fake_builder()
                    .context(context)
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn router_response_header() {
        let selector = RouterSelector::ResponseHeader {
            response_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .header("header_key", "header_value")
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "header_value".into()
        );

        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_request(
                &crate::services::RouterRequest::fake_builder()
                    .header("header_key", "header_value")
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn router_baggage() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let selector = RouterSelector::Baggage {
                baggage: "baggage_key".to_string(),
                redact: None,
                default: Some("defaulted".into()),
            };
            let span_context = SpanContext::new(
                TraceId::from_u128(42),
                SpanId::from_u64(42),
                // Make sure it's sampled if not, it won't create anything at the otel layer
                TraceFlags::default().with_sampled(true),
                false,
                TraceState::default(),
            );
            let _context_guard = Context::new()
                .with_remote_span_context(span_context)
                .with_baggage(vec![KeyValue::new("baggage_key", "baggage_value")])
                .attach();
            assert_eq!(
                selector
                    .on_request(
                        &crate::services::RouterRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                "defaulted".into()
            );

            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();
            assert_eq!(
                selector
                    .on_request(
                        &crate::services::RouterRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                "baggage_value".into()
            );
        });
    }

    #[test]
    fn router_trace_id() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let selector = RouterSelector::TraceId {
                trace_id: TraceIdFormat::Hexadecimal,
            };
            assert_eq!(
                selector.on_request(
                    &crate::services::RouterRequest::fake_builder()
                        .build()
                        .unwrap(),
                ),
                None
            );

            let span_context = SpanContext::new(
                TraceId::from_u128(42),
                SpanId::from_u64(42),
                TraceFlags::default().with_sampled(true),
                false,
                TraceState::default(),
            );
            let _context = Context::current()
                .with_remote_span_context(span_context)
                .attach();
            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();

            assert_eq!(
                selector
                    .on_request(
                        &crate::services::RouterRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                "0000000000000000000000000000002a".into()
            );

            let selector = RouterSelector::TraceId {
                trace_id: TraceIdFormat::Datadog,
            };

            assert_eq!(
                selector
                    .on_request(
                        &crate::services::RouterRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                opentelemetry::Value::String("42".into())
            );

            let selector = RouterSelector::TraceId {
                trace_id: TraceIdFormat::Uuid,
            };

            assert_eq!(
                selector
                    .on_request(
                        &crate::services::RouterRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                opentelemetry::Value::String("00000000-0000-0000-0000-00000000002a".into())
            );

            let selector = RouterSelector::TraceId {
                trace_id: TraceIdFormat::Decimal,
            };

            assert_eq!(
                selector
                    .on_request(
                        &crate::services::RouterRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                opentelemetry::Value::String("42".into())
            );
        });
    }

    #[test]
    fn test_router_studio_trace_id() {
        let selector = RouterSelector::StudioOperationId {
            studio_operation_id: true,
        };
        let ctx = crate::Context::new();
        let _ = ctx.insert(APOLLO_OPERATION_ID, "42".to_string()).unwrap();

        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .context(ctx)
                        .build()
                        .unwrap(),
                )
                .unwrap(),
            opentelemetry::Value::String("42".into())
        );
    }

    #[test]
    fn router_env() {
        let mut selector = RouterSelector::Env {
            env: "SELECTOR_ENV_VARIABLE".to_string(),
            redact: None,
            default: Some("defaulted".to_string()),
            mocked_env_var: None,
        };
        assert_eq!(
            selector.on_request(
                &crate::services::RouterRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("defaulted".into())
        );

        if let RouterSelector::Env { mocked_env_var, .. } = &mut selector {
            *mocked_env_var = Some("env_value".to_string())
        }
        assert_eq!(
            selector.on_request(
                &crate::services::RouterRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("env_value".into())
        );
    }

    #[test]
    fn router_operation_name_string() {
        let selector = RouterSelector::OperationName {
            operation_name: OperationName::String,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_response(
                &crate::services::RouterResponse::fake_builder()
                    .context(context.clone())
                    .build()
                    .unwrap(),
            ),
            Some("defaulted".into())
        );
        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        assert_eq!(
            selector.on_response(
                &crate::services::RouterResponse::fake_builder()
                    .context(context.clone())
                    .build()
                    .unwrap(),
            ),
            Some("topProducts".into())
        );
        assert_eq!(
            selector.on_error(&BoxError::from(String::from("my error")), &context),
            Some("topProducts".into())
        );
    }

    #[test]
    fn router_response_status_code() {
        let selector = RouterSelector::ResponseStatus {
            response_status: ResponseStatus::Code,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .status_code(StatusCode::NO_CONTENT)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            opentelemetry::Value::I64(204)
        );
    }

    #[test]
    fn router_response_status_reason() {
        let selector = RouterSelector::ResponseStatus {
            response_status: ResponseStatus::Reason,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::RouterResponse::fake_builder()
                        .status_code(StatusCode::NO_CONTENT)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "No Content".into()
        );
    }

    #[test]
    fn router_response_body() {
        let selector = RouterSelector::ResponseBody {
            response_body: true,
        };
        let res = &crate::services::RouterResponse::fake_builder()
            .status_code(StatusCode::OK)
            .data("some data")
            .build()
            .unwrap();
        assert_eq!(
            selector.on_response(res).unwrap().as_str(),
            r#"{"data":"some data"}"#
        );
    }

    #[test]
    fn router_response_body_errors() {
        let selector = RouterSelector::ResponseErrors {
            response_errors: JsonPathInst::new("$.[0]").unwrap(),
        };
        let res = &crate::services::RouterResponse::fake_builder()
            .status_code(StatusCode::BAD_REQUEST)
            .data("some data")
            .errors(vec![
                crate::graphql::Error::builder()
                    .message("Something went wrong")
                    .locations(vec![crate::graphql::Location { line: 1, column: 1 }])
                    .extension_code("GRAPHQL_VALIDATION_FAILED")
                    .build(),
            ])
            .build()
            .unwrap();
        assert_eq!(
            selector.on_response(res).unwrap().as_str(),
            r#"{"message":"Something went wrong","locations":[{"line":1,"column":1}],"extensions":{"code":"GRAPHQL_VALIDATION_FAILED"}}"#
        );
    }
}
