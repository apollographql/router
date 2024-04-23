use access_json::JSONQuery;
use derivative::Derivative;
use jsonpath_rust::JsonPathFinder;
use jsonpath_rust::JsonPathInst;
use schemars::JsonSchema;
use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use serde_json_bytes::ByteString;
use sha2::Digest;

use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugin::serde::deserialize_json_query;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::get_baggage;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::ToOtelValue;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TraceIdFormat {
    /// Open Telemetry trace ID, a hex string.
    OpenTelemetry,
    /// Datadog trace ID, a u64.
    Datadog,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize, PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize, PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Query {
    /// The raw query kind.
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize, PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ResponseStatus {
    /// The http status code.
    Code,
    /// The http status reason.
    Reason,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize, PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum RouterSelector {
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
    /// The request method.
    RequestMethod {
        /// The request method enabled or not
        request_method: bool,
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
    /// A status from the response
    ResponseStatus {
        /// The http response status code.
        response_status: ResponseStatus,
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
        #[allow(dead_code)]
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
    },
    Static(String),
    StaticField {
        /// A static string value
        r#static: String,
    },
    OnGraphQLError {
        /// Boolean set to true if the response body contains graphql error
        on_graphql_error: bool,
    },
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize, PartialEq))]
#[serde(deny_unknown_fields, untagged)]
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
        // Allow dead code is required because there is only one variant in Query and we need to avoid the dead code warning.
        #[allow(dead_code)]
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
    },
    Static(String),
    StaticField {
        /// A static string value
        r#static: String,
    },
}

#[derive(Deserialize, JsonSchema, Clone, Derivative)]
#[cfg_attr(test, derivative(PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
#[derivative(Debug)]
pub(crate) enum SubgraphSelector {
    SubgraphOperationName {
        /// The operation name from the subgraph query.
        subgraph_operation_name: OperationName,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphOperationKind {
        /// The kind of the subgraph operation (query|mutation|subscription).
        // Allow dead code is required because there is only one variant in OperationKind and we need to avoid the dead code warning.
        #[allow(dead_code)]
        subgraph_operation_kind: OperationKind,
    },
    SubgraphQuery {
        /// The graphql query to the subgraph.
        // Allow dead code is required because there is only one variant in Query and we need to avoid the dead code warning.
        #[allow(dead_code)]
        subgraph_query: Query,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphQueryVariable {
        /// The name of a subgraph query variable.
        subgraph_query_variable: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// Deprecated, use SubgraphResponseData and SubgraphResponseError instead
    SubgraphResponseBody {
        /// The subgraph response body json path.
        #[schemars(with = "String")]
        #[serde(deserialize_with = "deserialize_json_query")]
        subgraph_response_body: JSONQuery,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SubgraphResponseData {
        /// The subgraph response body json path.
        #[schemars(with = "String")]
        #[derivative(Debug = "ignore", PartialEq = "ignore")]
        #[serde(deserialize_with = "deserialize_jsonpath")]
        subgraph_response_data: JsonPathInst,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SubgraphResponseErrors {
        /// The subgraph response body json path.
        #[schemars(with = "String")]
        #[derivative(Debug = "ignore", PartialEq = "ignore")]
        #[serde(deserialize_with = "deserialize_jsonpath")]
        subgraph_response_errors: JsonPathInst,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SubgraphRequestHeader {
        /// The name of a subgraph request header.
        subgraph_request_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphResponseHeader {
        /// The name of a subgraph response header.
        subgraph_response_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphResponseStatus {
        /// The subgraph http response status code.
        subgraph_response_status: ResponseStatus,
    },
    SupergraphOperationName {
        /// The supergraph query operation name.
        supergraph_operation_name: OperationName,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphOperationKind {
        /// The supergraph query operation kind (query|mutation|subscription).
        // Allow dead code is required because there is only one variant in OperationKind and we need to avoid the dead code warning.
        #[allow(dead_code)]
        supergraph_operation_kind: OperationKind,
    },
    SupergraphQuery {
        /// The supergraph query to the subgraph.
        // Allow dead code is required because there is only one variant in Query and we need to avoid the dead code warning.
        #[allow(dead_code)]
        supergraph_query: Query,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphQueryVariable {
        /// The supergraph query variable name.
        supergraph_query_variable: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    SupergraphRequestHeader {
        /// The supergraph request header name.
        supergraph_request_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
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
    },
    Static(String),
    StaticField {
        /// A static string value
        r#static: String,
    },
}

impl Selector for RouterSelector {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> Option<opentelemetry::Value> {
        match self {
            RouterSelector::RequestMethod { request_method } if *request_method => {
                Some(request.router_request.method().to_string().into())
            }
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
            RouterSelector::Env { env, default, .. } => std::env::var(env)
                .ok()
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            RouterSelector::TraceId {
                trace_id: trace_id_format,
            } => trace_id().map(|id| {
                match trace_id_format {
                    TraceIdFormat::OpenTelemetry => id.to_string(),
                    TraceIdFormat::Datadog => id.to_datadog(),
                }
                .into()
            }),
            RouterSelector::Baggage {
                baggage, default, ..
            } => get_baggage(baggage).or_else(|| default.maybe_to_otel_value()),
            RouterSelector::Static(val) => Some(val.clone().into()),
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            // Related to Response
            _ => None,
        }
    }

    fn on_response(&self, response: &router::Response) -> Option<opentelemetry::Value> {
        match self {
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
            RouterSelector::Baggage {
                baggage, default, ..
            } => get_baggage(baggage).or_else(|| default.maybe_to_otel_value()),
            RouterSelector::OnGraphQLError { on_graphql_error } if *on_graphql_error => {
                if response.context.get_json_value(CONTAINS_GRAPHQL_ERROR)
                    == Some(serde_json_bytes::Value::Bool(true))
                {
                    Some(opentelemetry::Value::Bool(true))
                } else {
                    None
                }
            }
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }
}

impl Selector for SupergraphSelector {
    type Request = supergraph::Request;
    type Response = supergraph::Response;

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

            SupergraphSelector::Query { default, .. } => request
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

            SupergraphSelector::Env { env, default, .. } => std::env::var(env)
                .ok()
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SupergraphSelector::Static(val) => Some(val.clone().into()),
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            // For response
            _ => None,
        }
    }

    fn on_response(&self, response: &supergraph::Response) -> Option<opentelemetry::Value> {
        match self {
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
            SupergraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            // For request
            _ => None,
        }
    }
}

impl Selector for SubgraphSelector {
    type Request = subgraph::Request;
    type Response = subgraph::Response;

    fn on_request(&self, request: &subgraph::Request) -> Option<opentelemetry::Value> {
        match self {
            SubgraphSelector::SubgraphOperationName {
                subgraph_operation_name,
                default,
                ..
            } => {
                let op_name = request.subgraph_request.body().operation_name.clone();
                match subgraph_operation_name {
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
            SubgraphSelector::SupergraphOperationName {
                supergraph_operation_name,
                default,
                ..
            } => {
                let op_name = request.context.get(OPERATION_NAME).ok().flatten();
                match supergraph_operation_name {
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
            SubgraphSelector::SubgraphOperationKind { .. } => request
                .context
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SubgraphSelector::SupergraphOperationKind { .. } => request
                .context
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),

            SubgraphSelector::SupergraphQuery { default, .. } => request
                .supergraph_request
                .body()
                .query
                .clone()
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SubgraphSelector::SubgraphQuery { default, .. } => request
                .subgraph_request
                .body()
                .query
                .clone()
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SubgraphSelector::SubgraphQueryVariable {
                subgraph_query_variable,
                default,
                ..
            } => request
                .subgraph_request
                .body()
                .variables
                .get(&ByteString::from(subgraph_query_variable.as_str()))
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),

            SubgraphSelector::SupergraphQueryVariable {
                supergraph_query_variable,
                default,
                ..
            } => request
                .supergraph_request
                .body()
                .variables
                .get(&ByteString::from(supergraph_query_variable.as_str()))
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SubgraphSelector::SubgraphRequestHeader {
                subgraph_request_header,
                default,
                ..
            } => request
                .subgraph_request
                .headers()
                .get(subgraph_request_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SubgraphSelector::SupergraphRequestHeader {
                supergraph_request_header,
                default,
                ..
            } => request
                .supergraph_request
                .headers()
                .get(supergraph_request_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SubgraphSelector::RequestContext {
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
            SubgraphSelector::Baggage {
                baggage: baggage_name,
                default,
                ..
            } => get_baggage(baggage_name).or_else(|| default.maybe_to_otel_value()),

            SubgraphSelector::Env { env, default, .. } => std::env::var(env)
                .ok()
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SubgraphSelector::Static(val) => Some(val.clone().into()),
            SubgraphSelector::StaticField { r#static } => Some(r#static.clone().into()),

            // For response
            _ => None,
        }
    }

    fn on_response(&self, response: &subgraph::Response) -> Option<opentelemetry::Value> {
        match self {
            SubgraphSelector::SubgraphResponseHeader {
                subgraph_response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(subgraph_response_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            SubgraphSelector::SubgraphResponseStatus {
                subgraph_response_status: response_status,
            } => match response_status {
                ResponseStatus::Code => Some(opentelemetry::Value::I64(
                    response.response.status().as_u16() as i64,
                )),
                ResponseStatus::Reason => response
                    .response
                    .status()
                    .canonical_reason()
                    .map(|reason| reason.into()),
            },
            SubgraphSelector::SubgraphResponseBody {
                subgraph_response_body,
                default,
                ..
            } => subgraph_response_body
                .execute(response.response.body())
                .ok()
                .flatten()
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SubgraphSelector::SubgraphResponseData {
                subgraph_response_data,
                default,
                ..
            } => if let Some(data) = &response.response.body().data {
                let data: serde_json::Value = serde_json::to_value(data.clone()).ok()?;
                let mut val =
                    JsonPathFinder::new(Box::new(data), Box::new(subgraph_response_data.clone()))
                        .find();
                if let serde_json::Value::Array(array) = &mut val {
                    if array.len() == 1 {
                        val = array
                            .pop()
                            .expect("already checked the array had a length of 1; qed");
                    }
                }

                val.maybe_to_otel_value()
            } else {
                None
            }
            .or_else(|| default.maybe_to_otel_value()),
            SubgraphSelector::SubgraphResponseErrors {
                subgraph_response_errors: subgraph_response_error,
                default,
                ..
            } => {
                let errors = response.response.body().errors.clone();
                let data: serde_json::Value = serde_json::to_value(errors).ok()?;
                let mut val =
                    JsonPathFinder::new(Box::new(data), Box::new(subgraph_response_error.clone()))
                        .find();
                if let serde_json::Value::Array(array) = &mut val {
                    if array.len() == 1 {
                        val = array
                            .pop()
                            .expect("already checked the array had a length of 1; qed");
                    }
                }

                val.maybe_to_otel_value()
            }
            .or_else(|| default.maybe_to_otel_value()),
            SubgraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            SubgraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            // For request
            _ => None,
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::sync::Arc;

    use http::StatusCode;
    use jsonpath_rust::JsonPathInst;
    use opentelemetry::baggage::BaggageExt;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry_api::StringValue;
    use serde_json::json;
    use tracing::span;
    use tracing::subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::selectors::OperationKind;
    use crate::plugins::telemetry::config_new::selectors::OperationName;
    use crate::plugins::telemetry::config_new::selectors::Query;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
    use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
    use crate::plugins::telemetry::config_new::selectors::TraceIdFormat;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::plugins::telemetry::otel;

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
    fn subgraph_static() {
        let selector = SubgraphSelector::Static("test_static".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SubgraphRequest::fake_builder()
                        .supergraph_request(Arc::new(
                            http::Request::builder()
                                .body(crate::request::Request::builder().build())
                                .unwrap()
                        ))
                        .build()
                )
                .unwrap(),
            "test_static".into()
        );
    }

    #[test]
    fn subgraph_supergraph_request_header() {
        let selector = SubgraphSelector::SupergraphRequestHeader {
            supergraph_request_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SubgraphRequest::fake_builder()
                        .supergraph_request(Arc::new(
                            http::Request::builder()
                                .header("header_key", "header_value")
                                .body(crate::request::Request::builder().build())
                                .unwrap()
                        ))
                        .build()
                )
                .unwrap(),
            "header_value".into()
        );

        assert_eq!(
            selector
                .on_request(&crate::services::SubgraphRequest::fake_builder().build())
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake2_builder()
                    .header("header_key", "header_value")
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn subgraph_subgraph_request_header() {
        let selector = SubgraphSelector::SubgraphRequestHeader {
            subgraph_request_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SubgraphRequest::fake_builder()
                        .subgraph_request(
                            http::Request::builder()
                                .header("header_key", "header_value")
                                .body(graphql::Request::fake_builder().build())
                                .unwrap()
                        )
                        .build()
                )
                .unwrap(),
            "header_value".into()
        );

        assert_eq!(
            selector
                .on_request(&crate::services::SubgraphRequest::fake_builder().build())
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake2_builder()
                    .header("header_key", "header_value")
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn subgraph_subgraph_response_header() {
        let selector = SubgraphSelector::SubgraphResponseHeader {
            subgraph_response_header: "header_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake2_builder()
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
                    &crate::services::SubgraphResponse::fake2_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .subgraph_request(
                        http::Request::builder()
                            .header("header_key", "header_value")
                            .body(graphql::Request::fake_builder().build())
                            .unwrap()
                    )
                    .build()
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
    fn subgraph_request_context() {
        let selector = SubgraphSelector::RequestContext {
            request_context: "context_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        let _ = context.insert("context_key".to_string(), "context_value".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SubgraphRequest::fake_builder()
                        .context(context.clone())
                        .build()
                )
                .unwrap(),
            "context_value".into()
        );

        assert_eq!(
            selector
                .on_request(&crate::services::SubgraphRequest::fake_builder().build())
                .unwrap(),
            "defaulted".into()
        );
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake2_builder()
                    .context(context)
                    .build()
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn subgraph_response_context() {
        let selector = SubgraphSelector::ResponseContext {
            response_context: "context_key".to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        let _ = context.insert("context_key".to_string(), "context_value".to_string());
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake2_builder()
                        .context(context.clone())
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "context_value".into()
        );

        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake2_builder()
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );

        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context)
                    .build()
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
            let _context_guard = Context::new()
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
    fn supergraph_baggage() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let selector = SupergraphSelector::Baggage {
                baggage: "baggage_key".to_string(),
                redact: None,
                default: Some("defaulted".into()),
            };
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
    fn subgraph_baggage() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let selector = SubgraphSelector::Baggage {
                baggage: "baggage_key".to_string(),
                redact: None,
                default: Some("defaulted".into()),
            };
            assert_eq!(
                selector
                    .on_request(&crate::services::SubgraphRequest::fake_builder().build())
                    .unwrap(),
                "defaulted".into()
            );
            let _outer_guard = Context::new()
                .with_baggage(vec![KeyValue::new("baggage_key", "baggage_value")])
                .attach();

            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();

            assert_eq!(
                selector
                    .on_request(&crate::services::SubgraphRequest::fake_builder().build())
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
                trace_id: TraceIdFormat::OpenTelemetry,
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
                TraceFlags::default(),
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
        });
    }

    #[test]
    fn router_env() {
        let selector = RouterSelector::Env {
            env: "SELECTOR_ENV_VARIABLE".to_string(),
            redact: None,
            default: Some("defaulted".to_string()),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::RouterRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("defaulted".into())
        );
        // Env set
        std::env::set_var("SELECTOR_ENV_VARIABLE", "env_value");

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
    fn supergraph_env() {
        let selector = SupergraphSelector::Env {
            env: "SELECTOR_SUPERGRAPH_ENV_VARIABLE".to_string(),
            redact: None,
            default: Some("defaulted".to_string()),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("defaulted".into())
        );
        // Env set
        std::env::set_var("SELECTOR_SUPERGRAPH_ENV_VARIABLE", "env_value");

        assert_eq!(
            selector.on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap(),
            ),
            Some("env_value".into())
        );
        std::env::remove_var("SELECTOR_SUPERGRAPH_ENV_VARIABLE");
    }

    #[test]
    fn subgraph_env() {
        let selector = SubgraphSelector::Env {
            env: "SELECTOR_SUBGRAPH_ENV_VARIABLE".to_string(),
            redact: None,
            default: Some("defaulted".to_string()),
        };
        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build()),
            Some("defaulted".into())
        );
        // Env set
        std::env::set_var("SELECTOR_SUBGRAPH_ENV_VARIABLE", "env_value");

        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build()),
            Some("env_value".into())
        );
        std::env::remove_var("SELECTOR_SUBGRAPH_ENV_VARIABLE");
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
    fn subgraph_operation_kind() {
        let selector = SubgraphSelector::SupergraphOperationKind {
            supergraph_operation_kind: OperationKind::String,
        };
        let context = crate::context::Context::new();
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        // For now operation kind is contained in context
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context)
                    .build(),
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
    fn subgraph_supergraph_operation_name_string() {
        let selector = SubgraphSelector::SupergraphOperationName {
            supergraph_operation_name: OperationName::String,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context.clone())
                    .build(),
            ),
            Some("defaulted".into())
        );
        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        // For now operation kind is contained in context
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context)
                    .build(),
            ),
            Some("topProducts".into())
        );
    }

    #[test]
    fn subgraph_subgraph_operation_name_string() {
        let selector = SubgraphSelector::SubgraphOperationName {
            subgraph_operation_name: OperationName::String,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build(),),
            Some("defaulted".into())
        );
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .subgraph_request(
                        ::http::Request::builder()
                            .uri("http://localhost/graphql")
                            .body(
                                graphql::Request::fake_builder()
                                    .operation_name("topProducts")
                                    .build()
                            )
                            .unwrap()
                    )
                    .build(),
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
    fn subgraph_supergraph_operation_name_hash() {
        let selector = SubgraphSelector::SupergraphOperationName {
            supergraph_operation_name: OperationName::Hash,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context.clone())
                    .build(),
            ),
            Some("96294f50edb8f006f6b0a2dadae50d3c521e9841d07d6395d91060c8ccfed7f0".into())
        );

        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context)
                    .build(),
            ),
            Some("bd141fca26094be97c30afd42e9fc84755b252e7052d8c992358319246bd555a".into())
        );
    }

    #[test]
    fn subgraph_subgraph_operation_name_hash() {
        let selector = SubgraphSelector::SubgraphOperationName {
            subgraph_operation_name: OperationName::Hash,
            redact: None,
            default: Some("defaulted".to_string()),
        };
        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build()),
            Some("96294f50edb8f006f6b0a2dadae50d3c521e9841d07d6395d91060c8ccfed7f0".into())
        );

        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .subgraph_request(
                        ::http::Request::builder()
                            .uri("http://localhost/graphql")
                            .body(
                                graphql::Request::fake_builder()
                                    .operation_name("topProducts")
                                    .build()
                            )
                            .unwrap()
                    )
                    .build()
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

    #[test]
    fn subgraph_supergraph_query() {
        let selector = SubgraphSelector::SupergraphQuery {
            supergraph_query: Query::String,
            redact: None,
            default: Some("default".to_string()),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .supergraph_request(Arc::new(
                        http::Request::builder()
                            .body(
                                graphql::Request::fake_builder()
                                    .query("topProducts{name}")
                                    .build()
                            )
                            .unwrap()
                    ))
                    .build(),
            ),
            Some("topProducts{name}".into())
        );

        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build(),),
            Some("default".into())
        );
    }

    #[test]
    fn subgraph_subgraph_query() {
        let selector = SubgraphSelector::SubgraphQuery {
            subgraph_query: Query::String,
            redact: None,
            default: Some("default".to_string()),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .subgraph_request(
                        http::Request::builder()
                            .body(
                                graphql::Request::fake_builder()
                                    .query("topProducts{name}")
                                    .build()
                            )
                            .unwrap()
                    )
                    .build(),
            ),
            Some("topProducts{name}".into())
        );

        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build(),),
            Some("default".into())
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
    fn subgraph_subgraph_response_status_code() {
        let selector = SubgraphSelector::SubgraphResponseStatus {
            subgraph_response_status: ResponseStatus::Code,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .status_code(StatusCode::NO_CONTENT)
                        .build()
                )
                .unwrap(),
            opentelemetry::Value::I64(204)
        );
    }

    #[test]
    fn subgraph_subgraph_response_data() {
        let selector = SubgraphSelector::SubgraphResponseData {
            subgraph_response_data: JsonPathInst::from_str("$.hello").unwrap(),
            redact: None,
            default: None,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .data(serde_json_bytes::json!({
                            "hello": "bonjour"
                        }))
                        .build()
                )
                .unwrap(),
            opentelemetry::Value::String("bonjour".into())
        );

        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .data(serde_json_bytes::json!({
                            "hello": ["bonjour", "hello", "ciao"]
                        }))
                        .build()
                )
                .unwrap(),
            opentelemetry::Value::Array(
                vec![
                    StringValue::from("bonjour"),
                    StringValue::from("hello"),
                    StringValue::from("ciao")
                ]
                .into()
            )
        );

        assert!(selector
            .on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .data(serde_json_bytes::json!({
                        "hi": ["bonjour", "hello", "ciao"]
                    }))
                    .build()
            )
            .is_none());

        let selector = SubgraphSelector::SubgraphResponseData {
            subgraph_response_data: JsonPathInst::from_str("$.hello.*.greeting").unwrap(),
            redact: None,
            default: None,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .data(serde_json_bytes::json!({
                            "hello": {
                                "french": {
                                    "greeting": "bonjour"
                                },
                                "english": {
                                    "greeting": "hello"
                                },
                                "italian": {
                                    "greeting": "ciao"
                                }
                            }
                        }))
                        .build()
                )
                .unwrap(),
            opentelemetry::Value::Array(
                vec![
                    StringValue::from("bonjour"),
                    StringValue::from("hello"),
                    StringValue::from("ciao")
                ]
                .into()
            )
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
    fn subgraph_subgraph_response_status_reason() {
        let selector = SubgraphSelector::SubgraphResponseStatus {
            subgraph_response_status: ResponseStatus::Reason,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .status_code(StatusCode::NO_CONTENT)
                        .build()
                )
                .unwrap(),
            "No Content".into()
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

    #[test]
    fn subgraph_supergraph_query_variable() {
        let selector = SubgraphSelector::SupergraphQueryVariable {
            supergraph_query_variable: "key".to_string(),
            redact: None,
            default: Some(AttributeValue::String("default".to_string())),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .supergraph_request(Arc::new(
                        http::Request::builder()
                            .body(
                                graphql::Request::fake_builder()
                                    .variable("key", "value")
                                    .build()
                            )
                            .unwrap()
                    ))
                    .build(),
            ),
            Some("value".into())
        );

        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build(),),
            Some("default".into())
        );
    }

    #[test]
    fn subgraph_subgraph_query_variable() {
        let selector = SubgraphSelector::SubgraphQueryVariable {
            subgraph_query_variable: "key".to_string(),
            redact: None,
            default: Some("default".into()),
        };
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .subgraph_request(
                        http::Request::builder()
                            .body(
                                graphql::Request::fake_builder()
                                    .variable("key", "value")
                                    .build()
                            )
                            .unwrap()
                    )
                    .build(),
            ),
            Some("value".into())
        );

        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build(),),
            Some("default".into())
        );
    }
}
