use std::collections::HashMap;

use apollo_federation::connectors::ConnectorErrorsSettings;
use apollo_federation::connectors::HeaderSource;
use apollo_federation::connectors::HttpJsonTransport;
use apollo_federation::connectors::OriginatingDirective;
use apollo_federation::connectors::ProblemLocation;
use bytes::Bytes;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;

use crate::plugins::connectors::mapping::Problem;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ConnectorContext {
    items: Vec<ConnectorContextItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConnectorContextItem {
    problems: Vec<(ProblemLocation, Problem)>,
    request: ConnectorDebugHttpRequest,
    response: ConnectorDebugHttpResponse,
}

impl ConnectorContext {
    pub(crate) fn push_response(
        &mut self,
        request: Option<Box<ConnectorDebugHttpRequest>>,
        parts: &http::response::Parts,
        json_body: &serde_json_bytes::Value,
        selection_data: Option<SelectionData>,
        error_settings: &ConnectorErrorsSettings,
        problems: Vec<(ProblemLocation, Problem)>,
    ) {
        if let Some(request) = request {
            self.items.push(ConnectorContextItem {
                request: *request,
                response: ConnectorDebugHttpResponse::from((
                    parts,
                    json_body,
                    selection_data,
                    error_settings,
                )),
                problems,
            });
        } else {
            tracing::warn!(
                "connectors debugging: couldn't find a matching request for the response"
            );
        }
    }

    pub(crate) fn push_invalid_response(
        &mut self,
        request: Option<Box<ConnectorDebugHttpRequest>>,
        parts: &http::response::Parts,
        body: &Bytes,
        error_settings: &ConnectorErrorsSettings,
        problems: Vec<(ProblemLocation, Problem)>,
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
                                value.to_str().unwrap().to_string(),
                            )
                        })
                        .collect(),
                    body: ConnectorDebugBody {
                        kind: "invalid".to_string(),
                        content: format!("{:?}", body).into(),
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

    pub(super) fn serialize(self) -> serde_json_bytes::Value {
        json!(
            self.items
                .iter()
                .map(|item| {
                    // Items should be sorted so that they always come out in the same order
                    let mut problems = item.problems.clone();
                    problems.sort_by_key(|(location, _)| location.clone());

                    json!({
                        "request": item.request,
                        "response": item.response,
                        "problems": problems.iter().map(|(location, details)| json!({ "location": location, "details": details })).collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>()
        )
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
pub(crate) struct ConnectorDebugUri {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectorDebugHttpRequest {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<ConnectorDebugBody>,
    source_uri: Option<ConnectorDebugUri>,
    connect_uri: ConnectorDebugUri,
    source_headers: Option<Vec<(String, String)>>,
    connect_headers: Option<Vec<(String, String)>>,
}

impl
    From<(
        &http::Request<String>,
        String,
        Option<&serde_json_bytes::Value>,
        Option<SelectionData>,
        &HttpJsonTransport,
    )> for ConnectorDebugHttpRequest
{
    fn from(
        (req, kind, json_body, selection_data, transport): (
            &http::Request<String>,
            String,
            Option<&serde_json_bytes::Value>,
            Option<SelectionData>,
            &HttpJsonTransport,
        ),
    ) -> Self {
        let headers = transport.headers.iter().fold(
            HashMap::new(),
            |mut acc: HashMap<OriginatingDirective, Vec<(String, String)>>,
             (name, (source, directive))| {
                if let HeaderSource::Value(value) = source {
                    acc.entry(directive.clone())
                        .or_default()
                        .push((name.to_string(), value.to_string()));
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
                }),
            }),
            source_uri: if transport.source_url.is_some()
                || transport.source_path.is_some()
                || transport.source_query_params.is_some()
            {
                Some(ConnectorDebugUri {
                    base: transport.source_url.clone().map(|u| u.to_string()),
                    path: transport.source_path.clone().map(|u| u.to_string()),
                    query_params: transport.source_query_params.clone().map(|u| u.to_string()),
                })
            } else {
                None
            },
            connect_uri: ConnectorDebugUri {
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
struct ConnectorDebugHttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: ConnectorDebugBody,
    errors: Option<ConnectorDebugErrors>,
}

impl
    From<(
        &http::response::Parts,
        &serde_json_bytes::Value,
        Option<SelectionData>,
        &ConnectorErrorsSettings,
    )> for ConnectorDebugHttpResponse
{
    fn from(
        (parts, json_body, selection_data, error_settings): (
            &http::response::Parts,
            &serde_json_bytes::Value,
            Option<SelectionData>,
            &ConnectorErrorsSettings,
        ),
    ) -> Self {
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
