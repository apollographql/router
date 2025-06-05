use derivative::Derivative;
use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::path::JsonPathInst;
use sha2::Digest;

use super::attributes::SubgraphRequestResendCountKey;
use crate::Context;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugins::cache::entity::CacheSubgraph;
use crate::plugins::cache::metrics::CacheMetricContextKey;
use crate::plugins::limits::OperationLimits;
use crate::plugins::response_cache;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;
use crate::plugins::telemetry::config_new::ToOtelValue;
use crate::plugins::telemetry::config_new::get_baggage;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::plugins::telemetry::config_new::selectors::All;
use crate::plugins::telemetry::config_new::selectors::CacheKind;
use crate::plugins::telemetry::config_new::selectors::CacheStatus;
use crate::plugins::telemetry::config_new::selectors::EntityType;
use crate::plugins::telemetry::config_new::selectors::ErrorRepr;
use crate::plugins::telemetry::config_new::selectors::OperationKind;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::selectors::Query;
use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
use crate::services::subgraph;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SubgraphValue {
    Standard(Standard),
    Custom(Box<SubgraphSelector>),
}

impl From<&SubgraphValue> for InstrumentValue<SubgraphSelector> {
    fn from(value: &SubgraphValue) -> Self {
        match value {
            SubgraphValue::Standard(s) => InstrumentValue::Standard(s.clone()),
            SubgraphValue::Custom(selector) => InstrumentValue::Custom((**selector).clone()),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum SubgraphQuery {
    /// The raw query kind.
    String,
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
    SubgraphResendCount {
        /// The subgraph http resend count
        subgraph_resend_count: bool,
        /// Optional default value.
        default: Option<AttributeValue>,
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
    ResponseCache {
        /// Select if you want to get response cache hit or response cache miss
        response_cache: CacheKind,
        /// Specify the entity type on which you want the cache data. (default: all)
        entity_type: Option<EntityType>,
    },
    ResponseCacheStatus {
        /// Select if you want to know if it's a cache hit (all data coming from cache), miss (all data coming from subgraph) or partial_hit (not all entities are coming from cache for example)
        response_cache_status: CacheStatus,
        /// Specify the entity type on which you want the cache data status. (default: all)
        entity_type: Option<EntityType>,
    },
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
            SubgraphSelector::SubgraphName { subgraph_name } if *subgraph_name => {
                Some(request.subgraph_name.clone().into())
            }
            // .clone()
            // .map(opentelemetry::Value::from),
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

            SubgraphSelector::Env {
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
            SubgraphSelector::SubgraphOperationKind { .. } => response
                .context
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SubgraphSelector::SupergraphOperationKind { .. } => response
                .context
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SubgraphSelector::SupergraphOperationName {
                supergraph_operation_name,
                default,
                ..
            } => {
                let op_name = response.context.get(OPERATION_NAME).ok().flatten();
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
            SubgraphSelector::SubgraphName { subgraph_name } if *subgraph_name => {
                Some(response.subgraph_name.clone().into())
            }
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
            SubgraphSelector::SubgraphResendCount {
                subgraph_resend_count,
                default,
            } if *subgraph_resend_count => {
                response
                    .context
                    .get::<_, usize>(SubgraphRequestResendCountKey::new(&response.id))
                    .ok()
                    .flatten()
                    .map(|v| opentelemetry::Value::from(v as i64))
            }
            .or_else(|| default.maybe_to_otel_value()),
            SubgraphSelector::Static(val) => Some(val.clone().into()),
            SubgraphSelector::StaticField { r#static } => Some(r#static.clone().into()),
            SubgraphSelector::Cache { cache, entity_type } => {
                let cache_info: CacheSubgraph = response
                    .context
                    .get(CacheMetricContextKey::new(response.subgraph_name.clone()))
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
            SubgraphSelector::ResponseCache {
                response_cache: cache,
                entity_type,
            } => {
                let cache_info: response_cache::plugin::CacheSubgraph = response
                    .context
                    .get(response_cache::metrics::CacheMetricContextKey::new(
                        response.subgraph_name.clone(),
                    ))
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
            SubgraphSelector::ResponseCacheStatus {
                response_cache_status,
                entity_type,
            } => {
                let cache_info: response_cache::plugin::CacheSubgraph = response
                    .context
                    .get(response_cache::metrics::CacheMetricContextKey::new(
                        response.subgraph_name.clone(),
                    ))
                    .ok()
                    .flatten()?;

                let (cache_hit, cache_miss, entity_type_exist) = cache_info.0.iter().fold(
                    (0, 0, false),
                    |(mut cache_hit, mut cache_miss, mut entity_type_exist),
                     (current_entity_type, cache_hit_miss)| {
                        let compute = match entity_type {
                            Some(EntityType::All(All::All)) | None => true,
                            Some(EntityType::Named(entity_type_name)) => {
                                current_entity_type == entity_type_name
                            }
                        };
                        if compute {
                            cache_hit += cache_hit_miss.hit;
                            cache_miss += cache_hit_miss.miss;
                            entity_type_exist = true;
                        }

                        (cache_hit, cache_miss, entity_type_exist)
                    },
                );
                entity_type_exist.then(|| match response_cache_status {
                    CacheStatus::Hit => (cache_hit > 0 && cache_miss == 0).into(),
                    CacheStatus::Miss => (cache_hit == 0).into(),
                    CacheStatus::PartialHit => (cache_hit > 0 && cache_miss > 0).into(),
                    CacheStatus::Status => {
                        if cache_miss == 0 {
                            if cache_hit > 0 {
                                opentelemetry::Value::String("hit".into())
                            } else {
                                opentelemetry::Value::String("miss".into())
                            }
                        } else if cache_hit > 0 {
                            opentelemetry::Value::String("partial_hit".into())
                        } else {
                            opentelemetry::Value::String("miss".into())
                        }
                    }
                })
            }
            // For request
            _ => None,
        }
    }

    fn on_error(&self, error: &tower::BoxError, ctx: &Context) -> Option<opentelemetry::Value> {
        match self {
            SubgraphSelector::SubgraphOperationKind { .. } => ctx
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SubgraphSelector::SupergraphOperationKind { .. } => ctx
                .get::<_, String>(OPERATION_KIND)
                .ok()
                .flatten()
                .map(opentelemetry::Value::from),
            SubgraphSelector::SupergraphOperationName {
                supergraph_operation_name,
                default,
                ..
            } => {
                let op_name = ctx.get(OPERATION_NAME).ok().flatten();
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

    fn is_active(&self, stage: Stage) -> bool {
        match stage {
            Stage::Request => matches!(
                self,
                SubgraphSelector::SubgraphOperationName { .. }
                    | SubgraphSelector::SupergraphOperationName { .. }
                    | SubgraphSelector::SubgraphName { .. }
                    | SubgraphSelector::SubgraphOperationKind { .. }
                    | SubgraphSelector::SupergraphOperationKind { .. }
                    | SubgraphSelector::SupergraphQuery { .. }
                    | SubgraphSelector::SubgraphQuery { .. }
                    | SubgraphSelector::SubgraphQueryVariable { .. }
                    | SubgraphSelector::SupergraphQueryVariable { .. }
                    | SubgraphSelector::SubgraphRequestHeader { .. }
                    | SubgraphSelector::SupergraphRequestHeader { .. }
                    | SubgraphSelector::RequestContext { .. }
                    | SubgraphSelector::Baggage { .. }
                    | SubgraphSelector::Env { .. }
                    | SubgraphSelector::Static(_)
                    | SubgraphSelector::StaticField { .. }
            ),
            Stage::Response => matches!(
                self,
                SubgraphSelector::SubgraphResponseHeader { .. }
                    | SubgraphSelector::SubgraphResponseStatus { .. }
                    | SubgraphSelector::SubgraphOperationKind { .. }
                    | SubgraphSelector::SupergraphOperationKind { .. }
                    | SubgraphSelector::SupergraphOperationName { .. }
                    | SubgraphSelector::SubgraphName { .. }
                    | SubgraphSelector::SubgraphResponseData { .. }
                    | SubgraphSelector::SubgraphResponseErrors { .. }
                    | SubgraphSelector::ResponseContext { .. }
                    | SubgraphSelector::OnGraphQLError { .. }
                    | SubgraphSelector::Static(_)
                    | SubgraphSelector::StaticField { .. }
                    | SubgraphSelector::Cache { .. }
                    | SubgraphSelector::ResponseCache { .. }
            ),
            Stage::ResponseEvent => false,
            Stage::ResponseField => false,
            Stage::Error => matches!(
                self,
                SubgraphSelector::SubgraphOperationKind { .. }
                    | SubgraphSelector::SupergraphOperationKind { .. }
                    | SubgraphSelector::SupergraphOperationName { .. }
                    | SubgraphSelector::Error { .. }
                    | SubgraphSelector::Static(_)
                    | SubgraphSelector::StaticField { .. }
                    | SubgraphSelector::ResponseContext { .. }
            ),
            Stage::Drop => matches!(
                self,
                SubgraphSelector::Static(_) | SubgraphSelector::StaticField { .. }
            ),
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::sync::Arc;

    use http::StatusCode;
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry::StringValue;
    use opentelemetry::baggage::BaggageExt;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
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
    use crate::plugins::response_cache;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::plugins::telemetry::config_new::selectors::All;
    use crate::plugins::telemetry::config_new::selectors::CacheKind;
    use crate::plugins::telemetry::config_new::selectors::CacheStatus;
    use crate::plugins::telemetry::config_new::selectors::EntityType;
    use crate::plugins::telemetry::config_new::selectors::OperationKind;
    use crate::plugins::telemetry::config_new::selectors::OperationName;
    use crate::plugins::telemetry::config_new::selectors::Query;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphRequestResendCountKey;
    use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphQuery;
    use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
    use crate::plugins::telemetry::otel;
    use crate::services::subgraph::SubgraphRequestId;

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
    fn subgraph_resend_count() {
        let selector = SubgraphSelector::SubgraphResendCount {
            subgraph_resend_count: true,
            default: Some("defaulted".into()),
        };
        let context = crate::context::Context::new();
        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake2_builder()
                        .context(context.clone())
                        .build()
                        .unwrap()
                )
                .unwrap(),
            "defaulted".into()
        );
        let subgraph_req_id = SubgraphRequestId(String::from("test"));
        let _ = context.insert(SubgraphRequestResendCountKey::new(&subgraph_req_id), 2usize);

        assert_eq!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake2_builder()
                        .context(context.clone())
                        .id(subgraph_req_id)
                        .build()
                        .unwrap()
                )
                .unwrap(),
            2i64.into()
        );
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
    fn subgraph_env() {
        let mut selector = SubgraphSelector::Env {
            env: "SELECTOR_SUBGRAPH_ENV_VARIABLE".to_string(),
            redact: None,
            default: Some("defaulted".to_string()),
            mocked_env_var: None,
        };
        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build()),
            Some("defaulted".into())
        );

        if let SubgraphSelector::Env { mocked_env_var, .. } = &mut selector {
            *mocked_env_var = Some("env_value".to_string())
        }
        assert_eq!(
            selector.on_request(&crate::services::SubgraphRequest::fake_builder().build()),
            Some("env_value".into())
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
                    .context(context.clone())
                    .build(),
            ),
            Some("query".into())
        );
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
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
                    .context(context.clone())
                    .subgraph_name("test".to_string())
                    .build(),
            ),
            Some("test".into())
        );
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .context(context)
                    .subgraph_name("test".to_string())
                    .build(),
            ),
            Some("test".into())
        );
    }

    #[test]
    fn entity_cache_hit_all_entities() {
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
    fn response_cache_status_all() {
        let selector = SubgraphSelector::ResponseCacheStatus {
            response_cache_status: CacheStatus::Status,
            entity_type: Some(EntityType::All(All::All)),
        };
        let selector_hit = SubgraphSelector::ResponseCacheStatus {
            response_cache_status: CacheStatus::Hit,
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
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 3, miss: 1 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 2, miss: 0 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::String("partial_hit".into()))
        );

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
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 3, miss: 0 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 2, miss: 0 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::String("hit".into()))
        );
        assert_eq!(
            selector_hit.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::Bool(true))
        );
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 0, miss: 1 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 0, miss: 4 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::String("miss".into()))
        );
    }

    #[test]
    fn response_cache_status_type() {
        let selector = SubgraphSelector::ResponseCacheStatus {
            response_cache_status: CacheStatus::Status,
            entity_type: Some(EntityType::Named("Products".to_string())),
        };
        let selector_hit = SubgraphSelector::ResponseCacheStatus {
            response_cache_status: CacheStatus::Hit,
            entity_type: Some(EntityType::Named("Products".to_string())),
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
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 3, miss: 1 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 0, miss: 3 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::String("partial_hit".into()))
        );

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
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 3, miss: 0 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 2, miss: 1 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::String("hit".into()))
        );
        assert_eq!(
            selector_hit.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::Bool(true))
        );
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 0, miss: 1 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 4, miss: 4 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
            .unwrap();
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
                    .subgraph_name("test".to_string())
                    .context(context.clone())
                    .build(),
            ),
            Some(opentelemetry::Value::String("miss".into()))
        );
    }

    #[test]
    fn response_cache_hit_all_entities() {
        let selector = SubgraphSelector::ResponseCache {
            response_cache: CacheKind::Hit,
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
        let cache_info = response_cache::plugin::CacheSubgraph(
            [
                (
                    "Products".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 3, miss: 0 },
                ),
                (
                    "Reviews".to_string(),
                    response_cache::plugin::CacheHitMiss { hit: 2, miss: 0 },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let _ = context
            .insert(
                response_cache::metrics::CacheMetricContextKey::new("test".to_string()),
                cache_info,
            )
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
    fn response_cache_hit_one_entity() {
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
                    .context(context.clone())
                    .build(),
            ),
            Some("topProducts".into())
        );
        assert_eq!(
            selector.on_response(
                &crate::services::SubgraphResponse::fake_builder()
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

        assert!(
            selector
                .on_response(
                    &crate::services::SubgraphResponse::fake_builder()
                        .data(serde_json_bytes::json!({
                            "hi": ["bonjour", "hello", "ciao"]
                        }))
                        .build()
                )
                .is_none()
        );

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
