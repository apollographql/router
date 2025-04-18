use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_federation::sources::connect::HTTPMethod;
use apollo_federation::sources::connect::HeaderSource;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::connect::StringTemplate;
use displaydoc::Display;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Uri;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::uri::InvalidUri;
use http::uri::InvalidUriParts;
use http::uri::PathAndQuery;
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
    let uri = make_uri(
        transport.source_url.as_ref(),
        &transport.connect_template,
        &inputs,
    )?;

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
                    let len = encoded.bytes().len();
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

    match transport.method {
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
            serialize_request(
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
            )
        } else {
            serialize_request(
                &request,
                "json".to_string(),
                json_body.as_ref(),
                transport.body.as_ref().map(|body| SelectionData {
                    source: body.to_string(),
                    transformed: body.to_string(), // no transformation so this is the same
                    result: json_body.clone(),
                    errors: mapping_problems.clone(),
                }),
            )
        }
    });

    Ok((
        TransportRequest::Http(HttpRequest {
            inner: request,
            debug: debug_request,
        }),
        mapping_problems,
    ))
}

fn make_uri(
    source_url: Option<&Uri>,
    template: &StringTemplate,
    inputs: &IndexMap<String, Value>,
) -> Result<Uri, HttpJsonTransportError> {
    let connect_uri = template
        .interpolate_uri(inputs)
        .map_err(|err| HttpJsonTransportError::TemplateGenerationError(err.message))?;

    let Some(source_uri) = source_url else {
        return Ok(connect_uri);
    };

    let Some(connect_path_and_query) = connect_uri.path_and_query() else {
        return Ok(source_uri.clone());
    };

    // Extract source path and query
    let source_path = source_uri.path();
    let source_query = source_uri.query().unwrap_or("");

    // Extract connect path and query
    let connect_path = connect_path_and_query.path();
    let connect_query = connect_path_and_query.query().unwrap_or("");

    // Merge paths (ensuring proper slash handling)
    let merged_path = if connect_path.is_empty() || connect_path == "/" {
        source_path.to_string()
    } else if source_path.ends_with('/') {
        format!("{}{}", source_path, connect_path.trim_start_matches('/'))
    } else if connect_path.starts_with('/') {
        format!("{}{}", source_path, connect_path)
    } else {
        format!("{}/{}", source_path, connect_path)
    };

    // Merge query parameters
    let merged_query = if source_query.is_empty() {
        connect_query.to_string()
    } else if connect_query.is_empty() {
        source_query.to_string()
    } else {
        format!("{}&{}", source_query, connect_query)
    };

    // Build the merged URI
    let mut uri_parts = source_uri.clone().into_parts();
    let merged_path_and_query = if merged_query.is_empty() {
        merged_path
    } else {
        format!("{}?{}", merged_path, merged_query)
    };

    uri_parts.path_and_query = Some(PathAndQuery::from_str(&merged_path_and_query)?);

    // Reconstruct the URI and convert to string
    Uri::from_parts(uri_parts).map_err(HttpJsonTransportError::InvalidUri)
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

#[derive(Error, Display, Debug)]
pub(crate) enum HttpJsonTransportError {
    /// Error building URI: {0:?}
    NewUriError(#[from] Option<InvalidUri>),
    /// Could not generate HTTP request: {0}
    InvalidNewRequest(#[source] http::Error),
    /// Could not serialize body: {0}
    JsonBodySerialization(#[from] serde_json::Error),
    /// Could not serialize body: {0}
    FormBodySerialization(&'static str),
    /// Error building URI: {0:?}
    InvalidUri(#[from] InvalidUriParts),
    /// Could not generate URI from inputs: {0}
    TemplateGenerationError(String),
}

#[cfg(test)]
mod test_make_uri {
    use std::str::FromStr;

    use pretty_assertions::assert_eq;

    use super::*;

    macro_rules! this {
        ($($value:tt)*) => {{
            let mut map = IndexMap::with_capacity_and_hasher(1, Default::default());
            map.insert("$this".to_string(), json!({ $($value)* }));
            map
        }};
    }

    mod combining_paths {
        use pretty_assertions::assert_eq;
        use rstest::rstest;

        use super::*;
        #[rstest]
        #[case::connect_only("https://localhost:8080/v1", "/hello")]
        #[case::source_only("https://localhost:8080/v1/", "hello")]
        #[case::neither("https://localhost:8080/v1", "hello")]
        #[case::both("https://localhost:8080/v1/", "/hello")]
        fn slashes_between_source_and_connect(
            #[case] source_uri: &str,
            #[case] connect_path: &str,
        ) {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str(source_uri).unwrap()),
                    &connect_path.parse().unwrap(),
                    &Default::default(),
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/hello"
            );
        }

        #[test]
        fn preserve_trailing_slash_from_connect() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1").unwrap()),
                    &"/hello/".parse().unwrap(),
                    &Default::default(),
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/hello/"
            );
        }

        #[test]
        fn preserve_trailing_slash_from_source() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1/").unwrap()),
                    &"/".parse().unwrap(),
                    &Default::default(),
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/"
            );
        }

        #[test]
        fn preserve_no_trailing_slash_from_source() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1").unwrap()),
                    &"/".parse().unwrap(),
                    &Default::default(),
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1"
            );
        }

        #[test]
        fn add_path_before_query_params() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1?something").unwrap()),
                    &"/hello".parse().unwrap(),
                    &this! { "id": 42 },
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/hello?something"
            );
        }

        #[test]
        fn trailing_slash_plus_query_params() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1?something").unwrap()),
                    &"/hello/".parse().unwrap(),
                    &this! { "id": 42 },
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/hello/?something"
            );
        }

        #[test]
        fn with_merged_query_params() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1?foo=bar").unwrap()),
                    &"/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                    &this! {"id": 42 },
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/hello/42?foo=bar&id=42"
            );
        }
        #[test]
        fn with_trailing_slash_in_base_plus_query_params() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("https://localhost:8080/v1/?foo=bar").unwrap()),
                    &"/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                    &this! {"id": 42 },
                )
                .unwrap()
                .to_string(),
                "https://localhost:8080/v1/hello/42?foo=bar&id=42"
            );
        }
    }

    mod merge_query {
        use pretty_assertions::assert_eq;

        use super::*;
        #[test]
        fn source_only() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("http://localhost/users?a=b").unwrap()),
                    &"/123".parse().unwrap(),
                    &Default::default(),
                )
                .unwrap(),
                "http://localhost/users/123?a=b"
            );
        }

        #[test]
        fn connect_only() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("http://localhost/users").unwrap()),
                    &"?a=b&c=d".parse().unwrap(),
                    &Default::default(),
                )
                .unwrap(),
                "http://localhost/users?a=b&c=d"
            )
        }

        #[test]
        fn combine_from_both() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("http://localhost/users?a=b").unwrap()),
                    &"?c=d".parse().unwrap(),
                    &Default::default()
                )
                .unwrap(),
                "http://localhost/users?a=b&c=d"
            )
        }

        #[test]
        fn source_and_connect_have_same_param() {
            assert_eq!(
                make_uri(
                    Some(&Uri::from_str("http://localhost/users?a=b").unwrap()),
                    &"?a=d".parse().unwrap(),
                    &Default::default()
                )
                .unwrap(),
                "http://localhost/users?a=b&a=d"
            )
        }
    }

    #[test]
    fn fragments() {
        assert_eq!(
            make_uri(
                Some(&Uri::from_str("http://localhost/source?a=b#SourceFragment").unwrap()),
                &"/connect?c=d#connectFragment".parse().unwrap(),
                &Default::default()
            )
            .unwrap(),
            "http://localhost/source/connect?a=b&c=d"
        )
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HeaderSource;
    use apollo_federation::sources::connect::JSONSelection;
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
                headers: Default::default(),
                body: Some(JSONSelection::parse("$args { a }").unwrap()),
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

        let TransportRequest::Http(HttpRequest { inner: req, .. }) = req.0;
        let body = body::into_string(req.into_body()).await.unwrap();
        insta::assert_snapshot!(body, @r#"{"a":42}"#);
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

        let TransportRequest::Http(HttpRequest { inner: req, .. }) = req.0;
        let body = body::into_string(req.into_body()).await.unwrap();
        insta::assert_snapshot!(body, @r#"a=42"#);
    }
}
