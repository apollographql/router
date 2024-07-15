use std::collections::HashMap;

use apollo_federation::sources::connect::ApplyToError;
use apollo_federation::sources::connect::SubgraphConnectorConfiguration;
use bytes::Bytes;
use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use http::HeaderValue;
use itertools::Itertools;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceExt as TowerServiceExt;

use crate::layers::ServiceExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router::body::RouterBody;
use crate::services::supergraph;

const CONNECTORS_DEBUG_HEADER_NAME: &str = "Apollo-Connectors-Debugging";
const CONNECTORS_DEBUG_ENV: &str = "APOLLO_CONNECTORS_DEBUGGING";

#[derive(Debug, Clone)]
struct Connectors {
    debug_extensions: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectorsConfig {
    /// A map of subgraph name to connectors config for that subgraph
    #[serde(default)]
    pub(crate) subgraphs: HashMap<String, SubgraphConnectorConfiguration>,

    /// Enables connector debugging information on response extensions if the feature is enabled
    #[serde(default)]
    pub(crate) debug_extensions: bool,
}

#[async_trait::async_trait]
impl Plugin for Connectors {
    type Config = ConnectorsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let debug_extensions = init.config.debug_extensions
            || std::env::var(CONNECTORS_DEBUG_ENV).as_deref() == Ok("true");

        if debug_extensions {
            tracing::warn!(
                "Connector debugging is enabled, this may expose sensitive information."
            );
        }

        Ok(Connectors { debug_extensions })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let conf_enabled = self.debug_extensions;
        service
            .map_future_with_request_data(
                move |req: &supergraph::Request| {
                    let is_enabled = conf_enabled
                        && req
                            .supergraph_request
                            .headers()
                            .get(CONNECTORS_DEBUG_HEADER_NAME)
                            == Some(&HeaderValue::from_static("true"));
                    if is_enabled {
                        req.context.extensions().with_lock(|mut lock| {
                            lock.insert::<ConnectorContext>(ConnectorContext::default());
                        });
                    }

                    is_enabled
                },
                move |is_enabled: bool, f| async move {
                    let mut res: supergraph::ServiceResult = f.await;

                    res = match res {
                        Ok(mut res) => {
                            if is_enabled {
                                if let Some(debug) = res
                                    .context
                                    .extensions()
                                    .with_lock(|mut lock| lock.remove::<ConnectorContext>())
                                {
                                    let (parts, stream) = res.response.into_parts();
                                    let (mut first, rest) = stream.into_future().await;

                                    if let Some(first) = &mut first {
                                        first.extensions.insert(
                                            "apolloConnectorsDebugging",
                                            json!({"version": "1", "data": debug.serialize() }),
                                        );
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
}

register_plugin!("apollo", "preview_connectors", Connectors);

// === Structs for collecting debugging information ============================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ConnectorContext {
    requests: Vec<ConnectorDebugHttpRequest>,
    responses: Vec<ConnectorDebugHttpResponse>,
}

impl ConnectorContext {
    pub(crate) fn push_request(
        &mut self,
        req: &http::Request<RouterBody>,
        json_body: Option<&serde_json_bytes::Value>,
        selection_data: Option<SelectionData>,
    ) {
        self.requests
            .push(serialize_request(req, json_body, selection_data));
    }

    pub(crate) fn push_response(
        &mut self,
        parts: &http::response::Parts,
        json_body: &serde_json_bytes::Value,
        selection_data: Option<SelectionData>,
    ) {
        self.responses
            .push(serialize_response(parts, json_body, selection_data));
    }

    pub(crate) fn push_invalid_response(&mut self, parts: &http::response::Parts, body: &Bytes) {
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

pub(crate) struct SelectionData {
    pub(crate) source: String,
    pub(crate) transformed: String,
    pub(crate) result: Option<serde_json_bytes::Value>,
    pub(crate) errors: Vec<ApplyToError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugBody {
    kind: String,
    content: serde_json_bytes::Value,
    selection: Option<ConnectorDebugSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugHttpRequest {
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

fn serialize_request(
    req: &http::Request<RouterBody>,
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
            kind: "json".to_string(),
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
    let mut aggregated = vec![];

    for (key, group) in &errors.iter().group_by(|e| (e.message(), e.path())) {
        let group = group.collect_vec();
        aggregated.push(json!({
            "message": key.0,
            "path": key.1,
            "count": group.len(),
        }));
    }

    aggregated
}
