use crate::context::{OPERATION_KIND, OPERATION_NAME};
use crate::plugin::serde::deserialize_json_query;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::GetAttribute;
use crate::services::{router, subgraph, supergraph};
use crate::tracer::TraceId;
use access_json::JSONQuery;
use opentelemetry_api::baggage::BaggageExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json_bytes::ByteString;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum RouterSelector {
    /// A header from the request
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A header from the response
    ResponseHeader {
        /// The name of the request header.
        response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// The trace ID of the request.
    TraceId {
        /// The format of the trace ID.
        trace_id: TraceIdFormat,
    },
    /// A value from context.
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from baggage.
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
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
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum SupergraphSelector {
    OperationName {
        /// The operation name from the query.
        operation_name: OperationName,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    OperationKind {
        /// The operation kind from the query (query|mutation|subscription).
        operation_kind: OperationKind,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
    },
    Query {
        /// The graphql query.
        query: Query,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    QueryVariable {
        /// The name of a graphql query variable.
        query_variable: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseHeader {
        /// The name of the response header.
        response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    RequestContext {
        /// The request context key.
        request_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Env {
        /// The name of the environment variable
        env: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SubgraphSelector {
    SubgraphOperationName {
        /// The operation name from the subgraph query.
        subgraph_operation_name: OperationName,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphOperationKind {
        /// The kind of the subgraph operation (query|mutation|subscription).
        subgraph_operation_kind: OperationKind,
    },
    SubgraphQuery {
        /// The graphql query to the subgraph.
        subgraph_query: Query,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphQueryVariable {
        /// The name of a subgraph query variable.
        subgraph_query_variable: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SubgraphResponseBody {
        /// The subgraph response body json path.
        #[schemars(with = "String")]
        #[serde(deserialize_with = "deserialize_json_query")]
        subgraph_response_body: JSONQuery,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SubgraphRequestHeader {
        /// The name of the subgraph request header.
        subgraph_request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SubgraphResponseHeader {
        /// The name of the subgraph response header.
        subgraph_response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },

    SupergraphOperationName {
        /// The supergraph query operation name.
        supergraph_operation_name: OperationName,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphOperationKind {
        /// The supergraph query operation kind (query|mutation|subscription).
        supergraph_operation_kind: OperationKind,
    },
    SupergraphQuery {
        /// The supergraph query to the subgraph.
        supergraph_query: Query,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphQueryVariable {
        /// The supergraph query variable name.
        supergraph_query_variable: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SupergraphRequestHeader {
        /// The supergraph request header name.
        supergraph_request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SupergraphResponseHeader {
        /// The supergraph response header name.
        supergraph_response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    RequestContext {
        /// The request context key.
        request_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Env {
        /// The name of the environment variable
        env: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}

impl GetAttribute<router::Request, router::Response> for RouterSelector {
    fn on_request(&self, request: &router::Request) -> Option<AttributeValue> {
        match self {
            RouterSelector::RequestHeader {
                request_header,
                default,
                ..
            } => request
                .router_request
                .headers()
                .get(request_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            RouterSelector::Env { env, default, .. } => std::env::var(env)
                .ok()
                .map(AttributeValue::String)
                .or_else(|| default.clone().map(AttributeValue::String)),
            RouterSelector::TraceId {
                trace_id: trace_id_format,
            } => {
                let trace_id = TraceId::maybe_new()?;
                match trace_id_format {
                    TraceIdFormat::OpenTelemetry => AttributeValue::String(trace_id.to_string()),
                    TraceIdFormat::Datadog => AttributeValue::U128(trace_id.to_u128()),
                }
                .into()
            }
            RouterSelector::Baggage {
                baggage: baggage_name,
                default,
                ..
            } => {
                let span = Span::current();
                let span_context = span.context();
                // I must clone the key because the otel API is bad
                let baggage = span_context.baggage().get(baggage_name.clone()).cloned();
                match baggage {
                    Some(baggage) => AttributeValue::from(baggage).into(),
                    None => default.clone(),
                }
            }
            // Related to Response
            _ => None,
        }
    }

    fn on_response(&self, response: &router::Response) -> Option<AttributeValue> {
        match self {
            RouterSelector::ResponseHeader {
                response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(response_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            RouterSelector::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get(response_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            RouterSelector::Baggage {
                baggage: baggage_name,
                default,
                ..
            } => {
                let span = Span::current();
                let span_context = span.context();
                // I must clone the key because the otel API is bad
                let baggage = span_context.baggage().get(baggage_name.clone()).cloned();
                match baggage {
                    Some(baggage) => AttributeValue::from(baggage).into(),
                    None => default.clone(),
                }
            }
            _ => None,
        }
    }
}

impl GetAttribute<supergraph::Request, supergraph::Response> for SupergraphSelector {
    fn on_request(&self, request: &supergraph::Request) -> Option<AttributeValue> {
        match self {
            SupergraphSelector::OperationName {
                operation_name,
                default,
                ..
            } => {
                let op_name = request.context.get(OPERATION_NAME).ok().flatten();
                match operation_name {
                    OperationName::String => {
                        op_name.or_else(|| default.clone().map(AttributeValue::String))
                    }
                    OperationName::Hash => todo!(),
                }
            }
            SupergraphSelector::OperationKind { .. } => {
                request.context.get(OPERATION_KIND).ok().flatten()
            }
            SupergraphSelector::QueryVariable {
                query_variable,
                default,
                ..
            } => request
                .supergraph_request
                .body()
                .variables
                .get(&ByteString::from(query_variable.as_str()))
                .and_then(|v| serde_json::to_string(v).ok())
                .map(AttributeValue::String)
                .or_else(|| default.clone()),
            SupergraphSelector::RequestHeader {
                request_header,
                default,
                ..
            } => request
                .supergraph_request
                .headers()
                .get(request_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SupergraphSelector::RequestContext {
                request_context,
                default,
                ..
            } => request
                .context
                .get(request_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            SupergraphSelector::Baggage {
                baggage: baggage_name,
                default,
                ..
            } => {
                let span = Span::current();
                let span_context = span.context();
                // I must clone the key because the otel API is bad
                let baggage = span_context.baggage().get(baggage_name.clone()).cloned();
                match baggage {
                    Some(baggage) => AttributeValue::from(baggage).into(),
                    None => default.clone(),
                }
            }
            SupergraphSelector::Env { env, default, .. } => std::env::var(env)
                .ok()
                .map(AttributeValue::String)
                .or_else(|| default.clone().map(AttributeValue::String)),
            // For response
            _ => None,
        }
    }

    fn on_response(&self, response: &supergraph::Response) -> Option<AttributeValue> {
        match self {
            SupergraphSelector::ResponseHeader {
                response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(response_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SupergraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get(response_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            // For request
            _ => None,
        }
    }
}

impl GetAttribute<subgraph::Request, subgraph::Response> for SubgraphSelector {
    fn on_request(&self, request: &subgraph::Request) -> Option<AttributeValue> {
        match self {
            SubgraphSelector::SubgraphOperationName {
                subgraph_operation_name,
                default,
                ..
            } => {
                let op_name = request.subgraph_request.body().operation_name.clone();
                match subgraph_operation_name {
                    OperationName::String => op_name
                        .map(AttributeValue::String)
                        .or_else(|| default.clone().map(AttributeValue::String)),
                    OperationName::Hash => todo!(),
                }
            }
            SubgraphSelector::SupergraphOperationName {
                supergraph_operation_name,
                default,
                ..
            } => {
                let op_name = request.context.get(OPERATION_NAME).ok().flatten();
                match supergraph_operation_name {
                    OperationName::String => {
                        op_name.or_else(|| default.clone().map(AttributeValue::String))
                    }
                    OperationName::Hash => todo!(),
                }
            }
            SubgraphSelector::SubgraphOperationKind { .. } => AttributeValue::String(
                request
                    .operation_kind
                    .as_apollo_operation_type()
                    .to_string(),
            )
            .into(),
            SubgraphSelector::SupergraphOperationKind { .. } => {
                request.context.get(OPERATION_KIND).ok().flatten()
            }
            SubgraphSelector::SubgraphQueryVariable {
                subgraph_query_variable,
                default,
                ..
            } => request
                .subgraph_request
                .body()
                .variables
                .get(&ByteString::from(subgraph_query_variable.as_str()))
                .and_then(|v| serde_json::to_string(v).ok())
                .map(AttributeValue::String)
                .or_else(|| default.clone()),
            SubgraphSelector::SupergraphQueryVariable {
                supergraph_query_variable,
                default,
                ..
            } => request
                .supergraph_request
                .body()
                .variables
                .get(&ByteString::from(supergraph_query_variable.as_str()))
                .and_then(|v| serde_json::to_string(v).ok())
                .map(AttributeValue::String)
                .or_else(|| default.clone()),
            SubgraphSelector::SubgraphRequestHeader {
                subgraph_request_header,
                default,
                ..
            } => request
                .subgraph_request
                .headers()
                .get(subgraph_request_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SubgraphSelector::SupergraphRequestHeader {
                supergraph_request_header,
                default,
                ..
            } => request
                .supergraph_request
                .headers()
                .get(supergraph_request_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SubgraphSelector::RequestContext {
                request_context,
                default,
                ..
            } => request
                .context
                .get(request_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            SubgraphSelector::Baggage {
                baggage: baggage_name,
                default,
                ..
            } => {
                let span = Span::current();
                let span_context = span.context();
                // I must clone the key because the otel API is bad
                let baggage = span_context.baggage().get(baggage_name.clone()).cloned();
                match baggage {
                    Some(baggage) => AttributeValue::from(baggage).into(),
                    None => default.clone(),
                }
            }
            SubgraphSelector::Env { env, default, .. } => std::env::var(env)
                .ok()
                .map(AttributeValue::String)
                .or_else(|| default.clone().map(AttributeValue::String)),
            // For response
            _ => None,
        }
    }

    fn on_response(&self, response: &subgraph::Response) -> Option<AttributeValue> {
        match self {
            SubgraphSelector::SubgraphResponseHeader {
                subgraph_response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(subgraph_response_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SubgraphSelector::SubgraphResponseBody {
                subgraph_response_body,
                default,
                ..
            } => {
                let output = subgraph_response_body
                    .execute(response.response.body())
                    .ok()
                    .flatten()?;
                AttributeValue::try_from(output)
                    .ok()
                    .or_else(|| default.clone())
            }
            SubgraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get(response_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            // For request
            _ => None,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum RouterEvent {
    /// When a service request occurs.
    Request,
    /// When a service response occurs.
    Response,
    /// When a service error occurs.
    Error,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TraceIdFormat {
    /// Open Telemetry trace ID, a hex string.
    OpenTelemetry,
    /// Datadog trace ID, a u64.
    Datadog,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Query {
    /// The raw query kind.
    String,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}
