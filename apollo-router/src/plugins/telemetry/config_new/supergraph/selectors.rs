use derivative::Derivative;
use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::path::JsonPathInst;
use sha2::Digest;

use crate::Context;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugins::limits::OperationLimits;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;
use crate::plugins::telemetry::config_new::ToOtelValue;
use crate::plugins::telemetry::config_new::cost::CostValue;
use crate::plugins::telemetry::config_new::get_baggage;
use crate::plugins::telemetry::config_new::instruments::Event;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::plugins::telemetry::config_new::selectors::ErrorRepr;
use crate::plugins::telemetry::config_new::selectors::OperationKind;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::selectors::Query;
use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
use crate::services::FIRST_EVENT_CONTEXT_KEY;
use crate::services::supergraph;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SupergraphValue {
    Standard(Standard),
    Event(Event<SupergraphSelector>),
    Custom(SupergraphSelector),
}

impl From<&SupergraphValue> for InstrumentValue<SupergraphSelector> {
    fn from(value: &SupergraphValue) -> Self {
        match value {
            SupergraphValue::Standard(s) => InstrumentValue::Standard(s.clone()),
            SupergraphValue::Custom(selector) => match selector {
                SupergraphSelector::Cost { .. } => {
                    InstrumentValue::Chunked(Event::Custom(selector.clone()))
                }
                _ => InstrumentValue::Custom(selector.clone()),
            },
            SupergraphValue::Event(e) => InstrumentValue::Chunked(e.clone()),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Derivative)]
#[serde(deny_unknown_fields, untagged)]
#[derivative(Debug, PartialEq)]
pub(crate) enum SupergraphSelector {
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
    OperationKind {
        /// The operation kind from the query (query|mutation|subscription).
        // Allow dead code is required because there is only one variant in OperationKind and we need to avoid the dead code warning.
        #[allow(dead_code)]
        operation_kind: OperationKind,
    },
    Query {
        /// The graphql query.
        query: Query,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    QueryVariable {
        /// The name of a graphql query variable.
        query_variable: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    ResponseHeader {
        /// The name of the response header.
        response_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    /// A status from the response
    ResponseStatus {
        /// The http response status code.
        response_status: ResponseStatus,
    },
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
    ResponseData {
        /// The supergraph response body json path of the chunks.
        #[schemars(with = "String")]
        #[derivative(Debug = "ignore", PartialEq = "ignore")]
        #[serde(deserialize_with = "deserialize_jsonpath")]
        response_data: JsonPathInst,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseErrors {
        /// The supergraph response body json path of the chunks.
        #[schemars(with = "String")]
        #[derivative(Debug = "ignore", PartialEq = "ignore")]
        #[serde(deserialize_with = "deserialize_jsonpath")]
        response_errors: JsonPathInst,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
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
    /// Deprecated, should not be used anymore, use static field instead
    Static(String),
    StaticField {
        /// A static value
        r#static: AttributeValue,
    },
    OnGraphQLError {
        /// Boolean set to true if the response body contains graphql error
        on_graphql_error: bool,
    },
    Error {
        #[allow(dead_code)]
        /// Critical error if it happens
        error: ErrorRepr,
    },
    /// Cost attributes
    Cost {
        /// The cost value to select, one of: estimated, actual, delta.
        cost: CostValue,
    },
    /// Boolean returning true if it's the primary response and not events like subscription events or deferred responses
    IsPrimaryResponse {
        /// Boolean returning true if it's the primary response and not events like subscription events or deferred responses
        is_primary_response: bool,
    },
}

impl Selector for SupergraphSelector {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &supergraph::Request) -> Option<opentelemetry::Value> {
        match self {
            SupergraphSelector::OperationName {
                operation_name,
                default,
                ..
            } => {
                let op_name = request.context.get(OPERATION_NAME).ok().flatten();
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
            SupergraphSelector::OperationKind { .. } => request
                .context
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),

            SupergraphSelector::Query {
                default,
                query: Query::String,
                ..
            } => request
                .supergraph_request
                .body()
                .query
                .clone()
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SupergraphSelector::RequestHeader {
                request_header,
                default,
                ..
            } => request
                .supergraph_request
                .headers()
                .get(request_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SupergraphSelector::QueryVariable {
                query_variable,
                default,
                ..
            } => request
                .supergraph_request
                .body()
                .variables
                .get(&ByteString::from(query_variable.as_str()))
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::RequestContext {
                request_context,
                default,
                ..
            } => request
                .context
                .get::<_, serde_json_bytes::Value>(request_context)
                .ok()
                .flatten()
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::Baggage {
                baggage, default, ..
            } => get_baggage(baggage).or_else(|| default.maybe_to_otel_value()),

            SupergraphSelector::Env {
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
            SupergraphSelector::Static(val) => Some(val.clone().into()),
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            // For response
            _ => None,
        }
    }

    fn on_response(&self, response: &supergraph::Response) -> Option<opentelemetry::Value> {
        match self {
            SupergraphSelector::Query { query, .. } => {
                let limits_opt = response
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<OperationLimits>().cloned());
                match query {
                    Query::Aliases => {
                        limits_opt.map(|limits| opentelemetry::Value::I64(limits.aliases as i64))
                    }
                    Query::Depth => {
                        limits_opt.map(|limits| opentelemetry::Value::I64(limits.depth as i64))
                    }
                    Query::Height => {
                        limits_opt.map(|limits| opentelemetry::Value::I64(limits.height as i64))
                    }
                    Query::RootFields => limits_opt
                        .map(|limits| opentelemetry::Value::I64(limits.root_fields as i64)),
                    Query::String => None,
                }
            }
            SupergraphSelector::ResponseHeader {
                response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(response_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SupergraphSelector::ResponseStatus { response_status } => match response_status {
                ResponseStatus::Code => Some(opentelemetry::Value::I64(
                    response.response.status().as_u16() as i64,
                )),
                ResponseStatus::Reason => response
                    .response
                    .status()
                    .canonical_reason()
                    .map(|reason| reason.to_string().into()),
            },
            SupergraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::OnGraphQLError { on_graphql_error } if *on_graphql_error => {
                let contains_error = response
                    .context
                    .get_json_value(CONTAINS_GRAPHQL_ERROR)
                    .and_then(|value| value.as_bool())
                    .unwrap_or_default();
                Some(opentelemetry::Value::Bool(contains_error))
            }
            SupergraphSelector::OperationName {
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
            SupergraphSelector::OperationKind { .. } => response
                .context
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SupergraphSelector::IsPrimaryResponse {
                is_primary_response: is_primary,
            } if *is_primary => Some(true.into()),
            SupergraphSelector::Static(val) => Some(val.clone().into()),
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            // For request
            _ => None,
        }
    }

    fn on_response_event(
        &self,
        response: &Self::EventResponse,
        ctx: &Context,
    ) -> Option<opentelemetry::Value> {
        match self {
            SupergraphSelector::ResponseData {
                response_data,
                default,
                ..
            } => if let Some(data) = &response.data {
                let val = response_data.find(data);
                val.maybe_to_otel_value()
            } else {
                None
            }
            .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::ResponseErrors {
                response_errors,
                default,
                ..
            } => {
                let errors = response.errors.clone();
                let data: serde_json_bytes::Value = serde_json_bytes::to_value(errors).ok()?;
                let val = response_errors.find(&data);

                val.maybe_to_otel_value()
            }
            .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::Cost { cost } => match cost {
                CostValue::Estimated => ctx
                    .get_estimated_cost()
                    .ok()
                    .flatten()
                    .map(opentelemetry::Value::from),
                CostValue::Actual => ctx
                    .get_actual_cost()
                    .ok()
                    .flatten()
                    .map(opentelemetry::Value::from),
                CostValue::Delta => ctx
                    .get_cost_delta()
                    .ok()
                    .flatten()
                    .map(opentelemetry::Value::from),
                CostValue::Result => ctx
                    .get_cost_result()
                    .ok()
                    .flatten()
                    .map(opentelemetry::Value::from),
            },
            SupergraphSelector::OnGraphQLError { on_graphql_error } if *on_graphql_error => {
                let contains_error = ctx
                    .get_json_value(CONTAINS_GRAPHQL_ERROR)
                    .and_then(|value| value.as_bool())
                    .unwrap_or_default();
                Some(opentelemetry::Value::Bool(contains_error))
            }
            SupergraphSelector::OperationName {
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
            SupergraphSelector::OperationKind { .. } => ctx
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SupergraphSelector::IsPrimaryResponse {
                is_primary_response: is_primary,
            } if *is_primary => Some(opentelemetry::Value::Bool(
                ctx.get_json_value(FIRST_EVENT_CONTEXT_KEY)
                    == Some(serde_json_bytes::Value::Bool(true)),
            )),
            SupergraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => ctx
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::Static(val) => Some(val.clone().into()),
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn on_error(&self, error: &tower::BoxError, ctx: &Context) -> Option<opentelemetry::Value> {
        match self {
            SupergraphSelector::OperationName {
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
            SupergraphSelector::OperationKind { .. } => ctx
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SupergraphSelector::Query { query, .. } => {
                let limits_opt = ctx
                    .extensions()
                    .with_lock(|lock| lock.get::<OperationLimits>().cloned());
                match query {
                    Query::Aliases => {
                        limits_opt.map(|limits| opentelemetry::Value::I64(limits.aliases as i64))
                    }
                    Query::Depth => {
                        limits_opt.map(|limits| opentelemetry::Value::I64(limits.depth as i64))
                    }
                    Query::Height => {
                        limits_opt.map(|limits| opentelemetry::Value::I64(limits.height as i64))
                    }
                    Query::RootFields => limits_opt
                        .map(|limits| opentelemetry::Value::I64(limits.root_fields as i64)),
                    Query::String => None,
                }
            }
            SupergraphSelector::Error { .. } => Some(error.to_string().into()),
            SupergraphSelector::Static(val) => Some(val.clone().into()),
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            SupergraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => ctx
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SupergraphSelector::IsPrimaryResponse {
                is_primary_response: is_primary,
            } if *is_primary => Some(opentelemetry::Value::Bool(
                ctx.get_json_value(FIRST_EVENT_CONTEXT_KEY)
                    == Some(serde_json_bytes::Value::Bool(true)),
            )),
            _ => None,
        }
    }

    fn on_drop(&self) -> Option<Value> {
        match self {
            SupergraphSelector::Static(val) => Some(val.clone().into()),
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn is_active(&self, stage: Stage) -> bool {
        match stage {
            Stage::Request => matches!(
                self,
                SupergraphSelector::OperationName { .. }
                    | SupergraphSelector::OperationKind { .. }
                    | SupergraphSelector::Query { .. }
                    | SupergraphSelector::RequestHeader { .. }
                    | SupergraphSelector::QueryVariable { .. }
                    | SupergraphSelector::RequestContext { .. }
                    | SupergraphSelector::Baggage { .. }
                    | SupergraphSelector::Env { .. }
                    | SupergraphSelector::Static(_)
                    | SupergraphSelector::StaticField { .. }
            ),
            Stage::Response => matches!(
                self,
                SupergraphSelector::Query { .. }
                    | SupergraphSelector::ResponseHeader { .. }
                    | SupergraphSelector::ResponseStatus { .. }
                    | SupergraphSelector::ResponseContext { .. }
                    | SupergraphSelector::OnGraphQLError { .. }
                    | SupergraphSelector::OperationName { .. }
                    | SupergraphSelector::OperationKind { .. }
                    | SupergraphSelector::IsPrimaryResponse { .. }
                    | SupergraphSelector::Static(_)
                    | SupergraphSelector::StaticField { .. }
            ),
            Stage::ResponseEvent => matches!(
                self,
                SupergraphSelector::ResponseData { .. }
                    | SupergraphSelector::ResponseErrors { .. }
                    | SupergraphSelector::Cost { .. }
                    | SupergraphSelector::OnGraphQLError { .. }
                    | SupergraphSelector::OperationName { .. }
                    | SupergraphSelector::OperationKind { .. }
                    | SupergraphSelector::IsPrimaryResponse { .. }
                    | SupergraphSelector::ResponseContext { .. }
                    | SupergraphSelector::Static(_)
                    | SupergraphSelector::StaticField { .. }
            ),
            Stage::ResponseField => false,
            Stage::Error => matches!(
                self,
                SupergraphSelector::OperationName { .. }
                    | SupergraphSelector::OperationKind { .. }
                    | SupergraphSelector::Query { .. }
                    | SupergraphSelector::Error { .. }
                    | SupergraphSelector::Static(_)
                    | SupergraphSelector::StaticField { .. }
                    | SupergraphSelector::ResponseContext { .. }
                    | SupergraphSelector::IsPrimaryResponse { .. }
            ),
            Stage::Drop => matches!(
                self,
                SupergraphSelector::Static(_) | SupergraphSelector::StaticField { .. }
            ),
        }
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry::baggage::BaggageExt;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use tower::BoxError;
    use tracing::span;
    use tracing::subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::plugins::limits::OperationLimits;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::plugins::telemetry::config_new::selectors::OperationKind;
    use crate::plugins::telemetry::config_new::selectors::OperationName;
    use crate::plugins::telemetry::config_new::selectors::Query;
    use crate::plugins::telemetry::config_new::supergraph::selectors::SupergraphSelector;
    use crate::plugins::telemetry::otel;
    use crate::services::FIRST_EVENT_CONTEXT_KEY;

    #[test]
    fn supergraph_request_header() {
        let selector = SupergraphSelector::RequestHeader {
            request_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SupergraphRequest::fake_builder()
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
                    &crate::services::SupergraphRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_response(
                &crate::services::SupergraphResponse::fake_builder()
                    .header("header_key", "header_value")
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn supergraph_static() {
        let selector = SupergraphSelector::Static("test_static".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SupergraphRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "test_static".into()
        );
        assert_eq!(selector.on_drop().unwrap(), "test_static".into());
    }

    #[test]
    fn supergraph_static_field() {
        let selector = SupergraphSelector::StaticField {
            r#static: "test_static".to_string().into(),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SupergraphRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "test_static".into()
        );
        assert_eq!(selector.on_drop().unwrap(), "test_static".into());
    }

    #[test]
    fn supergraph_response_header() {
        let selector = SupergraphSelector::ResponseHeader {
            response_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
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
                    &crate::services::SupergraphResponse::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .header("header_key", "header_value")
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn supergraph_request_context() {
        let selector = SupergraphSelector::RequestContext {
            request_context: "context_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        let _ = context.insert("context_key".to_string(), "context_value".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SupergraphRequest::fake_builder()
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
                    &crate::services::SupergraphRequest::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );
        assert_eq!(
            selector.on_response(
                &crate::services::SupergraphResponse::fake_builder()
                    .context(context)
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn supergraph_is_primary() {
        let selector = SupergraphSelector::IsPrimaryResponse {
            is_primary_response: true,
        };
        let context = crate::context::Context::new();
        let _ = context.insert(FIRST_EVENT_CONTEXT_KEY, true);
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
                        .context(context.clone())
                        .build()
                        .unwrap()
                )
                .unwrap(),
            true.into()
        );
        assert_eq!(
            selector
                .on_response_event(&crate::graphql::Response::builder().build(), &context)
                .unwrap(),
            true.into()
        );
    }

    #[test]
    fn supergraph_response_context() {
        let selector = SupergraphSelector::ResponseContext {
            response_context: "context_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        let _ = context.insert("context_key".to_string(), "context_value".to_string());
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
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
                    &crate::services::SupergraphResponse::fake_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context)
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn supergraph_baggage() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let selector = SupergraphSelector::Baggage {
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
            assert_eq!(
                selector
                    .on_request(
                        &crate::services::SupergraphRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                "defaulted".into()
            );
            let _outer_guard = Context::new()
                .with_remote_span_context(span_context)
                .with_baggage(vec![KeyValue::new("baggage_key", "baggage_value")])
                .attach();
            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();

            assert_eq!(
                selector
                    .on_request(
                        &crate::services::SupergraphRequest::fake_builder()
                            .build()
                            .unwrap(),
                    )
                    .unwrap(),
                "baggage_value".into()
            );
        });
    }

    #[test]
    fn supergraph_env() {
        let mut selector = SupergraphSelector::Env {
            env: "SELECTOR_SUPERGRAPH_ENV_VARIABLE".to_string(),
            redact: None,
            default: Some("defaulted".to_string()),
            mocked_env_var: None,
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("defaulted".into())
        );

        if let SupergraphSelector::Env { mocked_env_var, .. } = &mut selector {
            *mocked_env_var = Some("env_value".to_string())
        }
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("env_value".into())
        );
    }

    #[test]
    fn supergraph_operation_kind() {
        let selector = SupergraphSelector::OperationKind {
            operation_kind: OperationKind::String,
        };
        let context = crate::context::Context::new();
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        // For now operation kind is contained in context
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context)
                    .build()
                    .unwrap(),
            ),
            Some("query".into())
        );
    }

    #[test]
    fn supergraph_operation_name_string() {
        let selector = SupergraphSelector::OperationName {
            operation_name: OperationName::String,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context.clone())
                    .build()
                    .unwrap(),
            ),
            Some("defaulted".into())
        );
        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        // For now operation kind is contained in context
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context)
                    .build()
                    .unwrap(),
            ),
            Some("topProducts".into())
        );
    }

    #[test]
    fn supergraph_operation_name_hash() {
        let selector = SupergraphSelector::OperationName {
            operation_name: OperationName::Hash,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context.clone())
                    .build()
                    .unwrap(),
            ),
            Some("96294f50edb8f006f6b0a2dadae50d3c521e9841d07d6395d91060c8ccfed7f0".into())
        );

        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context)
                    .build()
                    .unwrap(),
            ),
            Some("bd141fca26094be97c30afd42e9fc84755b252e7052d8c992358319246bd555a".into())
        );
    }

    #[test]
    fn supergraph_query() {
        let selector = SupergraphSelector::Query {
            query: Query::String,
            redact: None,
            default: Some("default".to_string()),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .query("topProducts{name}")
                    .build()
                    .unwrap(),
            ),
            Some("topProducts{name}".into())
        );

        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("default".into())
        );
    }

    fn create_select_and_context(query: Query) -> (SupergraphSelector, crate::Context) {
        let selector = SupergraphSelector::Query {
            query,
            redact: None,
            default: Some("default".to_string()),
        };
        let limits = OperationLimits {
            aliases: 1,
            depth: 2,
            height: 3,
            root_fields: 4,
        };
        let context = crate::Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<OperationLimits>(limits));
        (selector, context)
    }

    #[test]
    fn supergraph_query_aliases() {
        let (selector, context) = create_select_and_context(Query::Aliases);
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
                        .context(context)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            1.into()
        );
    }

    #[test]
    fn supergraph_query_depth() {
        let (selector, context) = create_select_and_context(Query::Depth);
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
                        .context(context)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            2.into()
        );
    }

    #[test]
    fn supergraph_query_height() {
        let (selector, context) = create_select_and_context(Query::Height);
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
                        .context(context)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            3.into()
        );
    }

    #[test]
    fn supergraph_query_root_fields() {
        let (selector, context) = create_select_and_context(Query::RootFields);
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SupergraphResponse::fake_builder()
                        .context(context.clone())
                        .build()
                        .unwrap()
                )
                .unwrap(),
            4.into()
        );
    }

    #[test]
    fn supergraph_query_variable() {
        let selector = SupergraphSelector::QueryVariable {
            query_variable: "key".to_string(),
            redact: None,
            default: Some(AttributeValue::String("default".to_string())),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .variable("key", "value")
                    .build()
                    .unwrap(),
            ),
            Some("value".into())
        );

        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("default".into())
        );
    }
}
