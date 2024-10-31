use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use apollo_federation::sources::connect::ApplyToError;
use bytes::Bytes;
use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use http::HeaderValue;
use itertools::Itertools;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as TowerServiceExt;

use super::query_plans::get_connectors;
use crate::layers::ServiceExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::connectors::configuration::ConnectorsConfig;
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::register_plugin;
use crate::services::execution;
use crate::services::router::body::RouterBody;
use crate::services::supergraph;

const CONNECTORS_DEBUG_HEADER_NAME: &str = "Apollo-Connectors-Debugging";
const CONNECTORS_DEBUG_ENV: &str = "APOLLO_CONNECTORS_DEBUGGING";
const CONNECTORS_DEBUG_KEY: &str = "apolloConnectorsDebugging";
const CONNECTORS_MAX_REQUESTS_ENV: &str = "APOLLO_CONNECTORS_MAX_REQUESTS_PER_OPERATION";
const CONNECTOR_SOURCES_IN_QUERY_PLAN: &str = "apollo_connectors::sources_in_query_plan";

static LAST_DEBUG_ENABLED_VALUE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
struct Connectors {
    debug_extensions: bool,
    max_requests: Option<usize>,
}

#[async_trait::async_trait]
impl Plugin for Connectors {
    type Config = ConnectorsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let debug_extensions = init.config.debug_extensions
            || std::env::var(CONNECTORS_DEBUG_ENV).as_deref() == Ok("true");

        let last_value = LAST_DEBUG_ENABLED_VALUE.load(Ordering::Relaxed);
        let swap_result = LAST_DEBUG_ENABLED_VALUE.compare_exchange(
            last_value,
            debug_extensions,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        // Ok means we swapped value, inner value is old value. Ok(false) means we went false -> true
        if matches!(swap_result, Ok(false)) {
            tracing::warn!(
                "Connector debugging is enabled, this may expose sensitive information."
            );
        }

        let max_requests = init
            .config
            .max_requests_per_operation_per_source
            .or(std::env::var(CONNECTORS_MAX_REQUESTS_ENV)
                .ok()
                .and_then(|v| v.parse().ok()));

        Ok(Connectors {
            debug_extensions,
            max_requests,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let conf_enabled = self.debug_extensions;
        let max_requests = self.max_requests;
        service
            .map_future_with_request_data(
                move |req: &supergraph::Request| {
                    let is_debug_enabled = conf_enabled
                        && req
                            .supergraph_request
                            .headers()
                            .get(CONNECTORS_DEBUG_HEADER_NAME)
                            == Some(&HeaderValue::from_static("true"));

                    req.context.extensions().with_lock(|mut lock| {
                        lock.insert::<Arc<RequestLimits>>(Arc::new(
                            RequestLimits::new(max_requests)
                        ));
                        if is_debug_enabled {
                            lock.insert::<Arc<Mutex<ConnectorContext>>>(Arc::new(Mutex::new(
                                ConnectorContext::default(),
                            )));
                        }
                    });

                    is_debug_enabled
                },
                move |is_debug_enabled: bool, f| async move {
                    let mut res: supergraph::ServiceResult = f.await;

                    res = match res {
                        Ok(mut res) => {
                            res.context.extensions().with_lock(|mut lock| {
                                if let Some(limits) = lock.remove::<Arc<RequestLimits>>() {
                                    limits.log();
                                }
                            });
                            if is_debug_enabled {
                                if let Some(debug) =
                                    res.context.extensions().with_lock(|mut lock| {
                                        lock.remove::<Arc<Mutex<ConnectorContext>>>()
                                    })
                                {
                                    let (parts, stream) = res.response.into_parts();
                                    let (mut first, rest) = stream.into_future().await;

                                    if let Some(first) = &mut first {
                                        if let Some(inner) = Arc::into_inner(debug) {
                                            first.extensions.insert(
                                                CONNECTORS_DEBUG_KEY,
                                                json!({"version": "1", "data": inner.into_inner().serialize() }),
                                            );
                                        }
                                    }
                                    res.response = http::Response::from_parts(
                                        parts,
                                        once(ready(first.unwrap_or_default())).chain(rest).boxed(),
                                    );
                                }
                            }

                            Ok(res)
                        }
                        Err(err) => Err(err),
                    };

                    res
                },
            )
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .map_request(|req: execution::Request| {
                let Some(connectors) = get_connectors(&req.context) else {
                    return req;
                };

                // add [{"subgraph_name": "", "source_name": ""}] to the context
                // for connectors with sources in the query plan.
                let list = req
                    .query_plan
                    .root
                    .service_usage()
                    .unique()
                    .flat_map(|service_name| {
                        let Some(connector) = connectors.get(service_name) else {
                            return None;
                        };

                        let Some(ref source_name) = connector.id.source_name else {
                            return None;
                        };

                        Some((connector.id.subgraph_name.clone(), source_name.clone()))
                    })
                    .unique()
                    .map(|(subgraph_name, source_name)| {
                        json!({
                            "subgraph_name": subgraph_name,
                            "source_name": source_name,
                        })
                    })
                    .collect_vec();

                req.context
                    .insert(CONNECTOR_SOURCES_IN_QUERY_PLAN, list)
                    .unwrap();
                req
            })
            .service(service)
            .boxed()
    }
}

pub(crate) const PLUGIN_NAME: &str = "preview_connectors";

register_plugin!("apollo", PLUGIN_NAME, Connectors);

// === Structs for collecting debugging information ============================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ConnectorContext {
    requests: Vec<ConnectorDebugHttpRequest>,
    responses: Vec<ConnectorDebugHttpResponse>,
}

impl ConnectorContext {
    pub(crate) fn push_response(
        &mut self,
        request: Option<ConnectorDebugHttpRequest>,
        parts: &http::response::Parts,
        json_body: &serde_json_bytes::Value,
        selection_data: Option<SelectionData>,
    ) {
        if let Some(request) = request {
            self.requests.push(request);
            self.responses
                .push(serialize_response(parts, json_body, selection_data));
        } else {
            tracing::warn!(
                "connectors debugging: couldn't find a matching request for the response"
            );
        }
    }

    pub(crate) fn push_invalid_response(
        &mut self,
        request: Option<ConnectorDebugHttpRequest>,
        parts: &http::response::Parts,
        body: &Bytes,
    ) {
        if let Some(request) = request {
            self.requests.push(request);
            self.responses.push(ConnectorDebugHttpResponse {
                status: parts.status.as_u16(),
                headers: parts
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap().to_string(),
                        )
                    })
                    .collect(),
                body: ConnectorDebugBody {
                    kind: "invalid".to_string(),
                    content: format!("{:?}", body).into(),
                    selection: None,
                },
            });
        } else {
            tracing::warn!(
                "connectors debugging: couldn't find a matching request for the response"
            );
        }
    }

    fn serialize(self) -> serde_json_bytes::Value {
        json!(self
            .requests
            .into_iter()
            .zip(self.responses.into_iter())
            .map(|(req, res)| json!({
                "request": req,
                "response": res,
            }))
            .collect::<Vec<_>>())
    }
}

/// JSONSelection Request / Response Data
///
/// Contains all needed info and responses from the application of a JSONSelection
pub(crate) struct SelectionData {
    /// The original [`JSONSelection`] to resolve
    pub(crate) source: String,

    /// A mapping of the original selection, taking into account renames and other
    /// transformations requested by the client
    ///
    /// Refer to [`Self::source`] for the original, schema-supplied selection.
    pub(crate) transformed: String,

    /// The result of applying the selection to JSON. An empty value
    /// here can potentially mean that errors were encountered.
    ///
    /// Refer to [`Self::errors`] for any errors found during evaluation
    pub(crate) result: Option<serde_json_bytes::Value>,

    /// A list of errors encountered during evaluation.
    pub(crate) errors: Vec<ApplyToError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugBody {
    kind: String,
    content: serde_json_bytes::Value,
    selection: Option<ConnectorDebugSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConnectorDebugHttpRequest {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<ConnectorDebugBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugSelection {
    source: String,
    transformed: String,
    result: Option<serde_json_bytes::Value>,
    errors: Vec<serde_json_bytes::Value>,
}

pub(crate) fn serialize_request(
    req: &http::Request<RouterBody>,
    kind: String,
    json_body: Option<&serde_json_bytes::Value>,
    selection_data: Option<SelectionData>,
) -> ConnectorDebugHttpRequest {
    ConnectorDebugHttpRequest {
        url: req.uri().to_string(),
        method: req.method().to_string(),
        headers: req
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap().to_string(),
                )
            })
            .collect(),
        body: json_body.map(|body| ConnectorDebugBody {
            kind,
            content: body.clone(),
            selection: selection_data.map(|selection| ConnectorDebugSelection {
                source: selection.source,
                transformed: selection.transformed,
                result: selection.result,
                errors: aggregate_apply_to_errors(&selection.errors),
            }),
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugHttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: ConnectorDebugBody,
}

fn serialize_response(
    parts: &http::response::Parts,
    json_body: &serde_json_bytes::Value,
    selection_data: Option<SelectionData>,
) -> ConnectorDebugHttpResponse {
    ConnectorDebugHttpResponse {
        status: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap().to_string(),
                )
            })
            .collect(),
        body: ConnectorDebugBody {
            kind: "json".to_string(),
            content: json_body.clone(),
            selection: selection_data.map(|selection| ConnectorDebugSelection {
                source: selection.source,
                transformed: selection.transformed,
                result: selection.result,
                errors: aggregate_apply_to_errors(&selection.errors),
            }),
        },
    }
}

fn aggregate_apply_to_errors(errors: &[ApplyToError]) -> Vec<serde_json_bytes::Value> {
    errors
        .iter()
        .fold(
            HashMap::default(),
            |mut acc: HashMap<(&str, String), usize>, err| {
                let path = err
                    .path()
                    .iter()
                    .map(|p| match p.as_u64() {
                        Some(_) => "@", // ignore array indices for grouping
                        None => p.as_str().unwrap_or_default(),
                    })
                    .join(".");

                acc.entry((err.message(), path))
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
                acc
            },
        )
        .iter()
        .map(|(key, count)| {
            json!({
                "message": key.0,
                "path": key.1,
                "count": count,
            })
        })
        .collect()
}
