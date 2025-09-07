use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use http::HeaderMap;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use parking_lot::Mutex;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use thiserror::Error;

use super::form_encoding::encode_json_as_form;
use crate::connectors::ApplyToError;
use crate::connectors::HTTPMethod;
use crate::connectors::Header;
use crate::connectors::HeaderSource;
use crate::connectors::HttpJsonTransport;
use crate::connectors::MakeUriError;
use crate::connectors::OriginatingDirective;
use crate::connectors::ProblemLocation;
use crate::connectors::runtime::debug::ConnectorContext;
use crate::connectors::runtime::debug::ConnectorDebugHttpRequest;
use crate::connectors::runtime::debug::DebugRequest;
use crate::connectors::runtime::debug::SelectionData;
use crate::connectors::runtime::mapping::Problem;
use crate::connectors::runtime::mapping::aggregate_apply_to_errors;
use crate::connectors::runtime::mapping::aggregate_apply_to_errors_with_problem_locations;

/// Request to an HTTP transport
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub inner: http::Request<String>,
    pub debug: DebugRequest,
}

/// Response from an HTTP transport
#[derive(Debug)]
pub struct HttpResponse {
    /// The response parts - the body is consumed by applying the JSON mapping
    pub inner: http::response::Parts,
}

/// Request to an underlying transport
#[derive(Debug, Clone)]
pub enum TransportRequest {
    /// A request to an HTTP transport
    Http(HttpRequest),
}

/// Response from an underlying transport
#[derive(Debug)]
pub enum TransportResponse {
    /// A response from an HTTP transport
    Http(HttpResponse),
}

impl TransportResponse {
    pub fn cache_policies(&self) -> HeaderMap {
        match self {
            TransportResponse::Http(http_response) => HeaderMap::from_iter(
                http_response
                    .inner
                    .headers
                    .get_all(CACHE_CONTROL)
                    .iter()
                    .map(|v| (CACHE_CONTROL, v.clone())),
            ),
        }
    }
}

impl From<HttpRequest> for TransportRequest {
    fn from(value: HttpRequest) -> Self {
        Self::Http(value)
    }
}

impl From<HttpResponse> for TransportResponse {
    fn from(value: HttpResponse) -> Self {
        Self::Http(value)
    }
}

pub fn make_request(
    transport: &HttpJsonTransport,
    inputs: IndexMap<String, Value>,
    client_headers: &HeaderMap<HeaderValue>,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<(TransportRequest, Vec<Problem>), HttpJsonTransportError> {
    let (uri, uri_apply_to_errors) = transport.make_uri(&inputs)?;
    let uri_mapping_problems =
        aggregate_apply_to_errors_with_problem_locations(uri_apply_to_errors);

    let method = transport.method;
    let request = http::Request::builder()
        .method(transport.method.as_str())
        .uri(uri);

    // add the headers and if content-type is specified, we'll check that when constructing the body
    let (mut request, content_type, header_apply_to_errors) =
        add_headers(request, client_headers, &transport.headers, &inputs);
    let header_mapping_problems =
        aggregate_apply_to_errors_with_problem_locations(header_apply_to_errors);

    let is_form_urlencoded = content_type.as_ref() == Some(&mime::APPLICATION_WWW_FORM_URLENCODED);

    let (json_body, form_body, body, content_length, body_apply_to_errors) =
        if let Some(ref selection) = transport.body {
            let (json_body, apply_to_errors) = selection.apply_with_vars(&json!({}), &inputs);
            let mut form_body = None;
            let (body, content_length) = if let Some(json_body) = json_body.as_ref() {
                if is_form_urlencoded {
                    let encoded = encode_json_as_form(json_body)
                        .map_err(HttpJsonTransportError::FormBodySerialization)?;
                    form_body = Some(encoded.clone());
                    let len = encoded.len();
                    (encoded, len)
                } else {
                    request = request.header(CONTENT_TYPE, mime::APPLICATION_JSON.essence_str());
                    let bytes = serde_json::to_vec(json_body)?;
                    let len = bytes.len();
                    let body_string = serde_json::to_string(json_body)?;
                    (body_string, len)
                }
            } else {
                ("".into(), 0)
            };
            (json_body, form_body, body, content_length, apply_to_errors)
        } else {
            (None, None, "".into(), 0, vec![])
        };

    match method {
        HTTPMethod::Post | HTTPMethod::Patch | HTTPMethod::Put => {
            request = request.header(CONTENT_LENGTH, content_length);
        }
        _ => {}
    }

    let request = request
        .body(body)
        .map_err(HttpJsonTransportError::InvalidNewRequest)?;

    let body_mapping_problems =
        aggregate_apply_to_errors(body_apply_to_errors, ProblemLocation::RequestBody);

    let all_problems: Vec<Problem> = uri_mapping_problems
        .chain(body_mapping_problems)
        .chain(header_mapping_problems)
        .collect();

    let debug_request = debug.as_ref().map(|_| {
        if is_form_urlencoded {
            Box::new(ConnectorDebugHttpRequest::new(
                &request,
                "form-urlencoded".to_string(),
                form_body.map(|s| Value::String(s.into())).as_ref(),
                transport.body.as_ref().map(|body| SelectionData {
                    source: body.to_string(),
                    transformed: body.to_string(), // no transformation so this is the same
                    result: json_body,
                }),
                transport,
            ))
        } else {
            Box::new(ConnectorDebugHttpRequest::new(
                &request,
                "json".to_string(),
                json_body.as_ref(),
                transport.body.as_ref().map(|body| SelectionData {
                    source: body.to_string(),
                    transformed: body.to_string(), // no transformation so this is the same
                    result: json_body.clone(),
                }),
                transport,
            ))
        }
    });

    Ok((
        TransportRequest::Http(HttpRequest {
            inner: request,
            debug: (debug_request, all_problems.clone()),
        }),
        all_problems,
    ))
}

fn add_headers(
    mut request: http::request::Builder,
    incoming_supergraph_headers: &HeaderMap<HeaderValue>,
    config: &[Header],
    inputs: &IndexMap<String, Value>,
) -> (
    http::request::Builder,
    Option<mime::Mime>,
    Vec<(ProblemLocation, ApplyToError)>,
) {
    let mut content_type = None;
    let mut warnings = Vec::new();

    for header in config {
        match &header.source {
            HeaderSource::From(from) => {
                let values = incoming_supergraph_headers.get_all(from);
                let mut propagated = false;
                for value in values {
                    request = request.header(header.name.clone(), value.clone());
                    propagated = true;
                }
                if !propagated {
                    tracing::warn!("Header '{}' not found in incoming request", header.name);
                }
            }
            HeaderSource::Value(value) => match value.interpolate(inputs) {
                Ok((value, apply_to_errors)) => {
                    warnings.extend(apply_to_errors.iter().cloned().map(|e| {
                        (
                            match header.originating_directive {
                                OriginatingDirective::Source => ProblemLocation::SourceHeaders,
                                OriginatingDirective::Connect => ProblemLocation::ConnectHeaders,
                            },
                            e,
                        )
                    }));

                    if header.name == CONTENT_TYPE {
                        content_type = Some(value.clone());
                    }

                    request = request.header(header.name.clone(), value);
                }
                Err(err) => {
                    tracing::error!("Unable to interpolate header value: {:?}", err);
                }
            },
        }
    }

    (
        request,
        content_type.and_then(|v| v.to_str().unwrap_or_default().parse().ok()),
        warnings,
    )
}

#[derive(Error, Debug)]
pub enum HttpJsonTransportError {
    #[error("Could not generate HTTP request: {0}")]
    InvalidNewRequest(#[source] http::Error),
    #[error("Could not serialize body: {0}")]
    JsonBodySerialization(#[from] serde_json::Error),
    #[error("Could not serialize body: {0}")]
    FormBodySerialization(&'static str),
    #[error(transparent)]
    MakeUri(#[from] MakeUriError),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use http::HeaderMap;
    use http::HeaderValue;
    use http::header::CONTENT_ENCODING;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::connectors::HTTPMethod;
    use crate::connectors::HeaderSource;
    use crate::connectors::JSONSelection;
    use crate::connectors::StringTemplate;

    #[test]
    fn test_headers_to_add_no_directives() {
        let incoming_supergraph_headers: HeaderMap<HeaderValue> = vec![
            ("x-rename".parse().unwrap(), "renamed".parse().unwrap()),
            ("x-rename".parse().unwrap(), "also-renamed".parse().unwrap()),
            ("x-ignore".parse().unwrap(), "ignored".parse().unwrap()),
            (CONTENT_ENCODING, "gzip".parse().unwrap()),
        ]
        .into_iter()
        .collect();

        let request = http::Request::builder();
        let (request, ..) = add_headers(
            request,
            &incoming_supergraph_headers,
            &[],
            &IndexMap::with_hasher(Default::default()),
        );
        let request = request.body("").unwrap();
        assert!(request.headers().is_empty());
    }

    #[test]
    fn test_headers_to_add_with_config() {
        let incoming_supergraph_headers: HeaderMap<HeaderValue> = vec![
            ("x-rename".parse().unwrap(), "renamed".parse().unwrap()),
            ("x-rename".parse().unwrap(), "also-renamed".parse().unwrap()),
            ("x-ignore".parse().unwrap(), "ignored".parse().unwrap()),
            (CONTENT_ENCODING, "gzip".parse().unwrap()),
        ]
        .into_iter()
        .collect();

        let config = vec![
            Header::from_values(
                "x-new-name".parse().unwrap(),
                HeaderSource::From("x-rename".parse().unwrap()),
                OriginatingDirective::Source,
            ),
            Header::from_values(
                "x-insert".parse().unwrap(),
                HeaderSource::Value("inserted".parse().unwrap()),
                OriginatingDirective::Connect,
            ),
        ];

        let request = http::Request::builder();
        let (request, ..) = add_headers(
            request,
            &incoming_supergraph_headers,
            &config,
            &IndexMap::with_hasher(Default::default()),
        );
        let request = request.body("").unwrap();
        let result = request.headers();
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("x-new-name"), Some(&"renamed".parse().unwrap()));
        assert_eq!(result.get("x-insert"), Some(&"inserted".parse().unwrap()));
    }

    #[test]
    fn make_request() {
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "a": 42 }));

        let req = super::make_request(
            &HttpJsonTransport {
                source_template: None,
                connect_template: StringTemplate::from_str("http://localhost:8080/").unwrap(),
                method: HTTPMethod::Post,
                body: Some(JSONSelection::parse("$args { a }").unwrap()),
                ..Default::default()
            },
            vars,
            &Default::default(),
            &None,
        )
        .unwrap();

        assert_debug_snapshot!(req, @r#"
        (
            Http(
                HttpRequest {
                    inner: Request {
                        method: POST,
                        uri: http://localhost:8080/,
                        version: HTTP/1.1,
                        headers: {
                            "content-type": "application/json",
                            "content-length": "8",
                        },
                        body: "{\"a\":42}",
                    },
                    debug: (
                        None,
                        [],
                    ),
                },
            ),
            [],
        )
        "#);

        let TransportRequest::Http(HttpRequest { inner: req, .. }) = req.0;
        let body = req.into_body();
        insta::assert_snapshot!(body, @r#"{"a":42}"#);
    }

    #[test]
    fn make_request_form_encoded() {
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "a": 42 }));
        let headers = vec![Header::from_values(
            "content-type".parse().unwrap(),
            HeaderSource::Value("application/x-www-form-urlencoded".parse().unwrap()),
            OriginatingDirective::Connect,
        )];

        let req = super::make_request(
            &HttpJsonTransport {
                source_template: None,
                connect_template: StringTemplate::from_str("http://localhost:8080/").unwrap(),
                method: HTTPMethod::Post,
                headers,
                body: Some(JSONSelection::parse("$args { a }").unwrap()),
                ..Default::default()
            },
            vars,
            &Default::default(),
            &None,
        )
        .unwrap();

        assert_debug_snapshot!(req, @r#"
        (
            Http(
                HttpRequest {
                    inner: Request {
                        method: POST,
                        uri: http://localhost:8080/,
                        version: HTTP/1.1,
                        headers: {
                            "content-type": "application/x-www-form-urlencoded",
                            "content-length": "4",
                        },
                        body: "a=42",
                    },
                    debug: (
                        None,
                        [],
                    ),
                },
            ),
            [],
        )
        "#);

        let TransportRequest::Http(HttpRequest { inner: req, .. }) = req.0;
        let body = req.into_body();
        insta::assert_snapshot!(body, @r#"a=42"#);
    }
}
