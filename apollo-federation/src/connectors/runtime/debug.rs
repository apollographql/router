use bytes::Bytes;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;

use crate::connectors::runtime::problem::Problem;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorContext {
    requests: Vec<ConnectorDebugHttpRequest>,
    responses: Vec<ConnectorDebugHttpResponse>,
}

impl ConnectorContext {
    pub fn push_response(
        &mut self,
        request: Option<Box<ConnectorDebugHttpRequest>>,
        parts: &http::response::Parts,
        json_body: &serde_json_bytes::Value,
        selection_data: Option<SelectionData>,
    ) {
        if let Some(request) = request {
            self.requests.push(*request);
            self.responses
                .push(serialize_response(parts, json_body, selection_data));
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
        body: &Bytes,
    ) {
        if let Some(request) = request {
            self.requests.push(*request);
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

    pub fn serialize(self) -> serde_json_bytes::Value {
        json!(
            self.requests
                .into_iter()
                .zip(self.responses.into_iter())
                .map(|(req, res)| json!({
                    "request": req,
                    "response": res,
                }))
                .collect::<Vec<_>>()
        )
    }
}

/// JSONSelection Request / Response Data
///
/// Contains all needed info and responses from the application of a JSONSelection
pub struct SelectionData {
    /// The original [`JSONSelection`] to resolve
    pub source: String,

    /// A mapping of the original selection, taking into account renames and other
    /// transformations requested by the client
    ///
    /// Refer to [`Self::source`] for the original, schema-supplied selection.
    pub transformed: String,

    /// The result of applying the selection to JSON. An empty value
    /// here can potentially mean that errors were encountered.
    ///
    /// Refer to [`Self::errors`] for any errors found during evaluation
    pub result: Option<serde_json_bytes::Value>,

    /// A list of mapping problems encountered during evaluation.
    pub errors: Vec<Problem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorDebugBody {
    kind: String,
    content: serde_json_bytes::Value,
    selection: Option<ConnectorDebugSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorDebugHttpRequest {
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
    errors: Vec<Problem>,
}

pub fn serialize_request(
    req: &http::Request<String>,
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
                errors: selection.errors,
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
                errors: selection.errors,
            }),
        },
    }
}
