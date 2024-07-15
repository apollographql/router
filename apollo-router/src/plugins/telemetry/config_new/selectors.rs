use access_json::JSONQuery;
use derivative::Derivative;
use opentelemetry_api::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::path::JsonPathInst;
use serde_json_bytes::ByteString;
use sha2::Digest;

use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugin::serde::deserialize_json_query;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugins::cache::entity::CacheSubgraph;
use crate::plugins::cache::metrics::CacheMetricContextKey;
use crate::plugins::demand_control::CostContext;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::cost::CostValue;
use crate::plugins::telemetry::config_new::get_baggage;
use crate::plugins::telemetry::config_new::instruments::Event;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::ToOtelValue;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::services::FIRST_EVENT_CONTEXT_KEY;
use crate::spec::operation_limits::OperationLimits;
use crate::Context;

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TraceIdFormat {
    /// Open Telemetry trace ID, a hex string.
    OpenTelemetry,
    /// Datadog trace ID, a u64.
    Datadog,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ErrorRepr {
    // /// The error code if available
    // Code,
    /// The error reason
    Reason,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Query {
    /// The raw query kind.
    String,
    /// The query aliases.
    Aliases,
    /// The query depth.
    Depth,
    /// The query height.
    Height,
    /// The query root fields.
    RootFields,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum SubgraphQuery {
    /// The raw query kind.
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ResponseStatus {
    /// The http status code.
    Code,
    /// The http status reason.
    Reason,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}

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

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
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
    /// Apollo Studio operation id
    StudioOperationId {
        /// Apollo Studio operation id
        studio_operation_id: bool,
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
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SupergraphValue {
    Standard(Standard),
    Event(Event<SupergraphSelector>),
    Custom(SupergraphSelector),
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum EventHolder {
    EventCustom(SupergraphSelector),
}

impl From<&SupergraphValue> for InstrumentValue<SupergraphSelector> {
    fn from(value: &SupergraphValue) -> Self {
        match value {
            SupergraphValue::Standard(s) => InstrumentValue::Standard(s.clone()),
            SupergraphValue::Custom(selector) => InstrumentValue::Custom(selector.clone()),
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

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SubgraphValue {
    Standard(Standard),
    Custom(SubgraphSelector),
}

impl From<&SubgraphValue> for InstrumentValue<SubgraphSelector> {
    fn from(value: &SubgraphValue) -> Self {
        match value {
            SubgraphValue::Standard(s) => InstrumentValue::Standard(s.clone()),
            SubgraphValue::Custom(selector) => InstrumentValue::Custom(selector.clone()),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Derivative)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
#[derivative(Debug, PartialEq)]
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
    SubgraphName {
        /// The subgraph name
        subgraph_name: bool,
    },
    SubgraphQuery {
        /// The graphql query to the subgraph.
        subgraph_query: SubgraphQuery,
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
    OnGraphQLError {
        /// Boolean set to true if the response body contains graphql error
        subgraph_on_graphql_error: bool,
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
    /// Deprecated, should not be used anymore, use static field instead
    Static(String),
    StaticField {
        /// A static value
        r#static: AttributeValue,
    },
    Error {
        /// Critical error if it happens
        error: ErrorRepr,
    },
    Cache {
        /// Select if you want to get cache hit or cache miss
        cache: CacheKind,
        /// Specify the entity type on which you want the cache data. (default: all)
        entity_type: Option<EntityType>,
    },
}

#[derive(Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case", untagged)]
pub(crate) enum EntityType {
    All(All),
    Named(String),
}

impl Default for EntityType {
    fn default() -> Self {
        Self::All(All::All)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum All {
    #[default]
    All,
}

#[derive(Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheKind {
    Hit,
    Miss,
}

impl Selector for RouterSelector {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

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
                if response.context.get_json_value(CONTAINS_GRAPHQL_ERROR)
                    == Some(serde_json_bytes::Value::Bool(true))
                {
                    Some(opentelemetry::Value::Bool(true))
                } else {
                    None
                }
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

    fn on_drop(&self) -> Option<Value> {
        match self {
            RouterSelector::Static(val) => Some(val.clone().into()),
            RouterSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }
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
            SupergraphSelector::Query { query, .. } => {
                let limits_opt = response
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<OperationLimits<u32>>().cloned());
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
                if response.context.get_json_value(CONTAINS_GRAPHQL_ERROR)
                    == Some(serde_json_bytes::Value::Bool(true))
                {
                    Some(opentelemetry::Value::Bool(true))
                } else {
                    None
                }
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
            SupergraphSelector::Cost { cost } => ctx.extensions().with_lock(|lock| {
                lock.get::<CostContext>().map(|cost_result| match cost {
                    CostValue::Estimated => cost_result.estimated.into(),
                    CostValue::Actual => cost_result.actual.into(),
                    CostValue::Delta => cost_result.delta().into(),
                    CostValue::Result => cost_result.result.into(),
                })
            }),
            SupergraphSelector::OnGraphQLError { on_graphql_error } if *on_graphql_error => {
                if ctx.get_json_value(CONTAINS_GRAPHQL_ERROR)
                    == Some(serde_json_bytes::Value::Bool(true))
                {
                    Some(opentelemetry::Value::Bool(true))
                } else {
                    None
                }
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
                    .with_lock(|lock| lock.get::<OperationLimits<u32>>().cloned());
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
}

impl Selector for SubgraphSelector {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

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
            SubgraphSelector::SubgraphName { subgraph_name } if *subgraph_name => request
                .subgraph_name
                .clone()
                .map(opentelemetry::Value::from),
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

            SubgraphSelector::SupergraphQuery {
                default,
                supergraph_query,
                ..
            } => {
                let limits_opt = request
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<OperationLimits<u32>>().cloned());
                match supergraph_query {
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
                    Query::String => request
                        .supergraph_request
                        .body()
                        .query
                        .clone()
                        .or_else(|| default.clone())
                        .map(opentelemetry::Value::from),
                }
            }
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
                let val = subgraph_response_data.find(data);

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
                let data: serde_json_bytes::Value = serde_json_bytes::to_value(errors).ok()?;

                let val = subgraph_response_error.find(&data);

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
            SubgraphSelector::OnGraphQLError {
                subgraph_on_graphql_error: on_graphql_error,
            } if *on_graphql_error => Some((!response.response.body().errors.is_empty()).into()),
            SubgraphSelector::Static(val) => Some(val.clone().into()),
            SubgraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            SubgraphSelector::Cache { cache, entity_type } => {
                let cache_info: CacheSubgraph = response
                    .context
                    .get(CacheMetricContextKey::new(response.subgraph_name.clone()?))
                    .ok()
                    .flatten()?;

                match entity_type {
                    Some(EntityType::All(All::All)) | None => Some(
                        (cache_info
                            .0
                            .iter()
                            .fold(0usize, |acc, (_entity_type, cache_hit_miss)| match cache {
                                CacheKind::Hit => acc + cache_hit_miss.hit,
                                CacheKind::Miss => acc + cache_hit_miss.miss,
                            }) as i64)
                            .into(),
                    ),
                    Some(EntityType::Named(entity_type_name)) => {
                        let res = cache_info.0.iter().fold(
                            0usize,
                            |acc, (entity_type, cache_hit_miss)| {
                                if entity_type == entity_type_name {
                                    match cache {
                                        CacheKind::Hit => acc + cache_hit_miss.hit,
                                        CacheKind::Miss => acc + cache_hit_miss.miss,
                                    }
                                } else {
                                    acc
                                }
                            },
                        );

                        (res != 0).then_some((res as i64).into())
                    }
                }
            }
            // For request
            _ => None,
        }
    }

    fn on_error(&self, error: &tower::BoxError, ctx: &Context) -> Option<opentelemetry::Value> {
        match self {
            SubgraphSelector::Error { .. } => Some(error.to_string().into()),
            SubgraphSelector::Static(val) => Some(val.clone().into()),
            SubgraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            SubgraphSelector::ResponseContext {
                response_context,
                default,
                ..
            } => ctx
                .get_json_value(response_context)
                .as_ref()
                .and_then(|v| v.maybe_to_otel_value())
                .or_else(|| default.maybe_to_otel_value()),
            _ => None,
        }
    }

    fn on_drop(&self) -> Option<Value> {
        match self {
            SubgraphSelector::Static(val) => Some(val.clone().into()),
            SubgraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::sync::Arc;

    use http::StatusCode;
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
    use serde_json_bytes::path::JsonPathInst;
    use tower::BoxError;
    use tracing::span;
    use tracing::subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::plugins::cache::entity::CacheHitMiss;
    use crate::plugins::cache::entity::CacheSubgraph;
    use crate::plugins::cache::metrics::CacheMetricContextKey;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::selectors::All;
    use crate::plugins::telemetry::config_new::selectors::CacheKind;
    use crate::plugins::telemetry::config_new::selectors::EntityType;
    use crate::plugins::telemetry::config_new::selectors::OperationKind;
    use crate::plugins::telemetry::config_new::selectors::OperationName;
    use crate::plugins::telemetry::config_new::selectors::Query;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::selectors::SubgraphQuery;
    use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
    use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
    use crate::plugins::telemetry::config_new::selectors::TraceIdFormat;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::plugins::telemetry::otel;
    use crate::query_planner::APOLLO_OPERATION_ID;
    use crate::services::FIRST_EVENT_CONTEXT_KEY;
    use crate::spec::operation_limits::OperationLimits;

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
    fn subgraph_static() {
        let selector = SubgraphSelector::Static("test_static".to_string());
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SubgraphRequest::fake_builder()
                        .supergraph_request(Arc::new(
                            http::Request::builder()
                                .body(graphql::Request::builder().build())
                                .unwrap()
                        ))
                        .build()
                )
                .unwrap(),
            "test_static".into()
        );
        assert_eq!(selector.on_drop().unwrap(), "test_static".into());
    }

    #[test]
    fn subgraph_static_field() {
        let selector = SubgraphSelector::StaticField {
            r#static: "test_static".to_string().into(),
        };
        assert_eq!(
            selector
                .on_request(
                    &crate::services::SubgraphRequest::fake_builder()
                        .supergraph_request(Arc::new(
                            http::Request::builder()
                                .body(graphql::Request::builder().build())
                                .unwrap()
                        ))
                        .build()
                )
                .unwrap(),
            "test_static".into()
        );
        assert_eq!(selector.on_drop().unwrap(), "test_static".into());
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
                                .body(graphql::Request::builder().build())
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
                .on_error(&BoxError::from(String::from("my error")), &context)
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
    fn subgraph_baggage() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let selector = SubgraphSelector::Baggage {
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
                    .on_request(&crate::services::SubgraphRequest::fake_builder().build())
                    .unwrap(),
                "defaulted".into()
            );
            let _outer_guard = Context::new()
                .with_baggage(vec![KeyValue::new("baggage_key", "baggage_value")])
                .with_remote_span_context(span_context)
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
    fn subgraph_name() {
        let selector = SubgraphSelector::SubgraphName {
            subgraph_name: true,
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_request(
                &crate::services::SubgraphRequest::fake_builder()
                    .context(context)
                    .subgraph_name("test".to_string())
                    .build(),
            ),
            Some("test".into())
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
    fn subgraph_cache_hit_all_entities() {
        let selector = SubgraphSelector::Cache {
            cache: CacheKind::Hit,
            entity_type: Some(EntityType::All(All::All)),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            None
        );
        let cache_info = CacheSubgraph(
            [
                ("Products".to_string(), CacheHitMiss { hit: 3, miss: 0 }),
                ("Reviews".to_string(), CacheHitMiss { hit: 2, miss: 0 }),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(CacheMetricContextKey::new("test".to_string()), cache_info)
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::I64(5))
        );
    }

    #[test]
    fn subgraph_cache_hit_one_entity() {
        let selector = SubgraphSelector::Cache {
            cache: CacheKind::Hit,
            entity_type: Some(EntityType::Named("Reviews".to_string())),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            None
        );
        let cache_info = CacheSubgraph(
            [
                ("Products".to_string(), CacheHitMiss { hit: 3, miss: 0 }),
                ("Reviews".to_string(), CacheHitMiss { hit: 2, miss: 0 }),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(CacheMetricContextKey::new("test".to_string()), cache_info)
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::I64(2))
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
            .with_lock(|mut lock| lock.insert::<OperationLimits<u32>>(limits));
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
                        .context(context)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            4.into()
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
            subgraph_query: SubgraphQuery::String,
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
    fn subgraph_on_graphql_error() {
        let selector = SubgraphSelector::OnGraphQLError {
            subgraph_on_graphql_error: true,
        };
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .error(
                            graphql::Error::builder()
                                .message("not found")
                                .extension_code("NOT_FOUND")
                                .build()
                        )
                        .build()
                )
                .unwrap(),
            opentelemetry::Value::Bool(true)
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
            opentelemetry::Value::Bool(false)
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
