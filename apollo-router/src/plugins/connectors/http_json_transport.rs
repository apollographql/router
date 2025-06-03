use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_federation::sources::connect::HTTPMethod;
use apollo_federation::sources::connect::HeaderSource;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::connect::MakeUriError;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use parking_lot::Mutex;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use thiserror::Error;

use super::form_encoding::encode_json_as_form;
use crate::plugins::connectors::mapping::Problem;
use crate::plugins::connectors::mapping::aggregate_apply_to_errors;
use crate::plugins::connectors::plugin::debug::ConnectorContext;
use crate::plugins::connectors::plugin::debug::SelectionData;
use crate::plugins::connectors::plugin::debug::serialize_request;
use crate::services::connect;
use crate::services::connector::request_service::TransportRequest;
use crate::services::connector::request_service::transport::http::HttpRequest;

pub(crate) fn make_request(
    transport: &HttpJsonTransport,
    inputs: IndexMap<String, Value>,
    original_request: &connect::Request,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<(TransportRequest, Vec<Problem>), HttpJsonTransportError> {
    let uri = transport.make_uri(&inputs)?;

    let method = transport.method;
    let request = http::Request::builder()
        .method(transport.method.as_str())
        .uri(uri);

    // add the headers and if content-type is specified, we'll check that when constructing the body
    let (mut request, content_type) = add_headers(
        request,
        original_request.supergraph_request.headers(),
        &transport.headers,
        &inputs,
    );

    let is_form_urlencoded = content_type.as_ref() == Some(&mime::APPLICATION_WWW_FORM_URLENCODED);

    let (json_body, form_body, body, content_length, apply_to_errors) =
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

    let mapping_problems = aggregate_apply_to_errors(&apply_to_errors);

    let debug_request = debug.as_ref().map(|_| {
        if is_form_urlencoded {
            Box::new(serialize_request(
                &request,
                "form-urlencoded".to_string(),
                form_body
                    .map(|s| serde_json_bytes::Value::String(s.clone().into()))
                    .as_ref(),
                transport.body.as_ref().map(|body| SelectionData {
                    source: body.to_string(),
                    transformed: body.to_string(), // no transformation so this is the same
                    result: json_body,
                    errors: mapping_problems.clone(),
                }),
            ))
        } else {
            Box::new(serialize_request(
                &request,
                "json".to_string(),
                json_body.as_ref(),
                transport.body.as_ref().map(|body| SelectionData {
                    source: body.to_string(),
                    transformed: body.to_string(), // no transformation so this is the same
                    result: json_body.clone(),
                    errors: mapping_problems.clone(),
                }),
            ))
        }
    });

    Ok((
        TransportRequest::Http(Box::new(HttpRequest {
            inner: request,
            debug: debug_request,
        })),
        mapping_problems,
    ))
}

#[allow(clippy::mutable_key_type)] // HeaderName is internally mutable, but safe to use in maps
fn add_headers(
    mut request: http::request::Builder,
    incoming_supergraph_headers: &HeaderMap<HeaderValue>,
    config: &IndexMap<HeaderName, HeaderSource>,
    inputs: &IndexMap<String, Value>,
) -> (http::request::Builder, Option<mime::Mime>) {
    let mut content_type = None;

    for (header_name, header_source) in config {
        match header_source {
            HeaderSource::From(from) => {
                let values = incoming_supergraph_headers.get_all(from);
                let mut propagated = false;
                for value in values {
                    request = request.header(header_name.clone(), value.clone());
                    propagated = true;
                }
                if !propagated {
                    tracing::warn!("Header '{}' not found in incoming request", header_name);
                }
            }
            HeaderSource::Value(value) => match value.interpolate(inputs) {
                Ok(value) => {
                    request = request.header(header_name, value.clone());

                    if header_name == CONTENT_TYPE {
                        content_type = Some(value.clone());
                    }
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
    )
}

#[derive(Error, Debug)]
pub(crate) enum HttpJsonTransportError {
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

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HeaderSource;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::StringTemplate;
    use http::HeaderMap;
    use http::HeaderValue;
    use http::header::CONTENT_ENCODING;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::Context;
    use crate::services::router::body;

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
        let (request, _) = add_headers(
            request,
            &incoming_supergraph_headers,
            &IndexMap::with_hasher(Default::default()),
            &IndexMap::with_hasher(Default::default()),
        );
        let request = request.body(body::empty()).unwrap();
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

        #[allow(clippy::mutable_key_type)]
        let mut config = IndexMap::with_hasher(Default::default());
        config.insert(
            "x-new-name".parse().unwrap(),
            HeaderSource::From("x-rename".parse().unwrap()),
        );
        config.insert(
            "x-insert".parse().unwrap(),
            HeaderSource::Value("inserted".parse().unwrap()),
        );

        let request = http::Request::builder();
        let (request, _) = add_headers(
            request,
            &incoming_supergraph_headers,
            &config,
            &IndexMap::with_hasher(Default::default()),
        );
        let request = request.body(body::empty()).unwrap();
        let result = request.headers();
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("x-new-name"), Some(&"renamed".parse().unwrap()));
        assert_eq!(result.get("x-insert"), Some(&"inserted".parse().unwrap()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn make_request() {
        let schema = Schema::parse_and_validate("type Query { f(a: Int): String }", "").unwrap();
        let doc = ExecutableDocument::parse_and_validate(&schema, "{f(a: 42)}", "").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "a": 42 }));

        let req = super::make_request(
            &HttpJsonTransport {
                source_url: None,
                connect_template: StringTemplate::from_str("http://localhost:8080/").unwrap(),
                method: HTTPMethod::Post,
                body: Some(JSONSelection::parse("$args { a }").unwrap()),
                ..Default::default()
            },
            vars,
            &connect::Request {
                service_name: Arc::from("service"),
                context: Context::default(),
                operation: Arc::from(doc),
                supergraph_request: Arc::from(http::Request::default()),
                variables: Default::default(),
                keys: Default::default(),
            },
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
                    debug: None,
                },
            ),
            [],
        )
        "#);

        if let TransportRequest::Http(req) = req.0 {
            let req = req.inner;
            let body = body::into_string(req.into_body()).await.unwrap();
            insta::assert_snapshot!(body, @r#"{"a":42}"#);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn make_request_form_encoded() {
        let schema = Schema::parse_and_validate("type Query { f(a: Int): String }", "").unwrap();
        let doc = ExecutableDocument::parse_and_validate(&schema, "{f(a: 42)}", "").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "a": 42 }));
        let mut headers = IndexMap::default();
        headers.insert(
            "content-type".parse().unwrap(),
            HeaderSource::Value("application/x-www-form-urlencoded".parse().unwrap()),
        );

        let req = super::make_request(
            &HttpJsonTransport {
                source_url: None,
                connect_template: StringTemplate::from_str("http://localhost:8080/").unwrap(),
                method: HTTPMethod::Post,
                headers,
                body: Some(JSONSelection::parse("$args { a }").unwrap()),
                ..Default::default()
            },
            vars,
            &connect::Request {
                service_name: Arc::from("service"),
                context: Context::default(),
                operation: Arc::from(doc),
                supergraph_request: Arc::from(http::Request::default()),
                variables: Default::default(),
                keys: Default::default(),
            },
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
                    debug: None,
                },
            ),
            [],
        )
        "#);

        if let TransportRequest::Http(req) = req.0 {
            let req = req.inner;
            let body = body::into_string(req.into_body()).await.unwrap();
            insta::assert_snapshot!(body, @r#"a=42"#);
        }
    }
}
