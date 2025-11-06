use std::collections::HashMap;

use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;

use crate::connectors::ConnectorErrorsSettings;
use crate::connectors::HeaderSource;
use crate::connectors::HttpJsonTransport;
use crate::connectors::OriginatingDirective;
use crate::connectors::runtime::mapping::Problem;

#[derive(Debug, Clone, Default)]
pub struct ConnectorContext {
    items: Vec<ConnectorContextItem>,
}

#[derive(Debug, Clone)]
pub struct ConnectorContextItem {
    problems: Vec<Problem>,
    request: ConnectorDebugHttpRequest,
    response: ConnectorDebugHttpResponse,
}

impl ConnectorContext {
    pub fn push_response(
        &mut self,
        request: Option<Box<ConnectorDebugHttpRequest>>,
        parts: &http::response::Parts,
        json_body: &serde_json_bytes::Value,
        selection_data: Option<SelectionData>,
        error_settings: &ConnectorErrorsSettings,
        problems: Vec<Problem>,
    ) {
        if let Some(request) = request {
            self.items.push(ConnectorContextItem {
                request: *request,
                response: ConnectorDebugHttpResponse::new(
                    parts,
                    json_body,
                    selection_data,
                    error_settings,
                ),
                problems,
            });
        } else {
            tracing::warn!(
                "connectors debugging: couldn't find a matching request for the response"
            );
        }
    }

    pub fn push_invalid_response(
        &mut self,
        request: Option<Box<ConnectorDebugHttpRequest>>,
        parts: &http::response::Parts,
        body: &[u8],
        error_settings: &ConnectorErrorsSettings,
        problems: Vec<Problem>,
    ) {
        if let Some(request) = request {
            self.items.push(ConnectorContextItem {
                request: *request,
                response: ConnectorDebugHttpResponse {
                    status: parts.status.as_u16(),
                    headers: parts
                        .headers
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.as_str().to_string(),
                                value.to_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect(),
                    body: ConnectorDebugBody {
                        kind: "invalid".to_string(),
                        content: format!("{body:?}").into(),
                        selection: None,
                    },
                    errors: if error_settings.message.is_some()
                        || error_settings.connect_extensions.is_some()
                        || error_settings.source_extensions.is_some()
                    {
                        Some(ConnectorDebugErrors {
                            message: error_settings.message.as_ref().map(|m| m.to_string()),
                            source_extensions: error_settings
                                .source_extensions
                                .as_ref()
                                .map(|m| m.to_string()),
                            connect_extensions: error_settings
                                .connect_extensions
                                .as_ref()
                                .map(|m| m.to_string()),
                        })
                    } else {
                        None
                    },
                },
                problems,
            });
        } else {
            tracing::warn!(
                "connectors debugging: couldn't find a matching request for the response"
            );
        }
    }

    pub fn serialize(self) -> serde_json_bytes::Value {
        json!(
            self.items
                .iter()
                .map(|item| {
                    // Items should be sorted so that they always come out in the same order
                    let problems = item
                        .problems
                        .iter()
                        .sorted_by_key(|problem| problem.location)
                        .map(
                            |Problem {
                                 message,
                                 path,
                                 count,
                                 location,
                             }| {
                                // This is the format the Sandbox Debugger expects, don't change
                                // it without updating that project
                                json!({
                                    "location": location,
                                    "details": {
                                        "message": message,
                                        "path": path,
                                        "count": count,
                                    },
                                })
                            },
                        )
                        .collect_vec();

                    json!({
                        "request": item.request,
                        "response": item.response,
                        "problems": problems
                    })
                })
                .collect::<Vec<_>>()
        )
    }

    pub fn problems(&self) -> Vec<serde_json_bytes::Value> {
        self.items
            .iter()
            .flat_map(|item| item.problems.iter())
            .map(|problem| json!({ "message": problem.message, "path": problem.path }))
            .collect()
    }
}

/// JSONSelection Request / Response Data
///
/// Contains all needed info and responses from the application of a JSONSelection
pub struct SelectionData {
    /// The original JSONSelection to resolve
    pub source: String,

    /// A mapping of the original selection, taking into account renames and other
    /// transformations requested by the client
    ///
    /// Refer to [`Self::source`] for the original, schema-supplied selection.
    pub transformed: String,

    /// The result of applying the selection to JSON. An empty value
    /// here can potentially mean that errors were encountered.
    pub result: Option<serde_json_bytes::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugBody {
    kind: String,
    content: serde_json_bytes::Value,
    selection: Option<ConnectorDebugSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugSelection {
    source: String,
    transformed: String,
    result: Option<serde_json_bytes::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorDebugUri {
    base: Option<String>,
    path: Option<String>,
    query_params: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectorDebugErrors {
    message: Option<String>,
    source_extensions: Option<String>,
    connect_extensions: Option<String>,
}

pub type DebugRequest = (Option<Box<ConnectorDebugHttpRequest>>, Vec<Problem>);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorDebugHttpRequest {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<ConnectorDebugBody>,
    source_url: Option<ConnectorDebugUri>,
    connect_url: ConnectorDebugUri,
    source_headers: Option<Vec<(String, String)>>,
    connect_headers: Option<Vec<(String, String)>>,
}

impl ConnectorDebugHttpRequest {
    pub fn new(
        req: &http::Request<String>,
        kind: String,
        json_body: Option<&serde_json_bytes::Value>,
        selection_data: Option<SelectionData>,
        transport: &HttpJsonTransport,
    ) -> Self {
        let headers = transport.headers.iter().fold(
            HashMap::new(),
            |mut acc: HashMap<OriginatingDirective, Vec<(String, String)>>, header| {
                if let HeaderSource::Value(value) = &header.source {
                    acc.entry(header.originating_directive)
                        .or_default()
                        .push((header.name.to_string(), value.to_string()));
                }
                acc
            },
        );

        ConnectorDebugHttpRequest {
            url: req.uri().to_string(),
            method: req.method().to_string(),
            headers: req
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
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
                }),
            }),
            source_url: if transport.source_template.is_some()
                || transport.source_path.is_some()
                || transport.source_query_params.is_some()
            {
                Some(ConnectorDebugUri {
                    base: transport.source_template.clone().map(|u| u.to_string()),
                    path: transport.source_path.clone().map(|u| u.to_string()),
                    query_params: transport.source_query_params.clone().map(|u| u.to_string()),
                })
            } else {
                None
            },
            connect_url: ConnectorDebugUri {
                base: Some(transport.connect_template.clone().to_string()),
                path: transport.connect_path.clone().map(|u| u.to_string()),
                query_params: transport
                    .connect_query_params
                    .clone()
                    .map(|u| u.to_string()),
            },
            connect_headers: headers.get(&OriginatingDirective::Connect).cloned(),
            source_headers: headers.get(&OriginatingDirective::Source).cloned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorDebugHttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: ConnectorDebugBody,
    errors: Option<ConnectorDebugErrors>,
}

impl ConnectorDebugHttpResponse {
    pub fn new(
        parts: &http::response::Parts,
        json_body: &serde_json_bytes::Value,
        selection_data: Option<SelectionData>,
        error_settings: &ConnectorErrorsSettings,
    ) -> Self {
        ConnectorDebugHttpResponse {
            status: parts.status.as_u16(),
            headers: parts
                .headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
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
                }),
            },
            errors: if error_settings.message.is_some()
                || error_settings.connect_extensions.is_some()
                || error_settings.source_extensions.is_some()
            {
                Some(ConnectorDebugErrors {
                    message: error_settings.message.as_ref().map(|m| m.to_string()),
                    source_extensions: error_settings
                        .source_extensions
                        .as_ref()
                        .map(|m| m.to_string()),
                    connect_extensions: error_settings
                        .connect_extensions
                        .as_ref()
                        .map(|m| m.to_string()),
                })
            } else {
                None
            },
        }
    }
}
