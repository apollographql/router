use std::collections::HashSet;
use std::iter::Iterator;
use std::str::FromStr;
use std::sync::Arc;

use apollo_federation::sources::connect::ApplyTo;
use apollo_federation::sources::connect::HTTPHeader;
use apollo_federation::sources::connect::HttpJsonTransport;
use displaydoc::Display;
use http::header::ACCEPT;
use http::header::ACCEPT_ENCODING;
use http::header::CONNECTION;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::header::HOST;
use http::header::PROXY_AUTHENTICATE;
use http::header::PROXY_AUTHORIZATION;
use http::header::TE;
use http::header::TRAILER;
use http::header::TRANSFER_ENCODING;
use http::header::UPGRADE;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use lazy_static::lazy_static;
use percent_encoding::percent_decode_str;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use thiserror::Error;
use url::Url;

use crate::error::ConnectorDirectiveError;
use crate::plugins::connectors::plugin::ConnectorContext;
use crate::plugins::connectors::plugin::SelectionData;
use crate::services::connect;
use crate::services::router::body::RouterBody;

static KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");

// Copied from plugins::headers
// Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
// These are not propagated by default using a regex match as they will not make sense for the
// second hop.
// In addition, because our requests are not regular proxy requests content-type, content-length
// and host are also in the exclude list.
lazy_static! {
    static ref RESERVED_HEADERS: Arc<HashSet<&'static HeaderName>> = Arc::new(HashSet::from([
        &CONNECTION,
        &PROXY_AUTHENTICATE,
        &PROXY_AUTHORIZATION,
        &TE,
        &TRAILER,
        &TRANSFER_ENCODING,
        &UPGRADE,
        &CONTENT_LENGTH,
        &CONTENT_TYPE,
        &CONTENT_ENCODING,
        &HOST,
        &ACCEPT,
        &ACCEPT_ENCODING,
        &KEEP_ALIVE,
    ]));
}

pub(crate) fn make_request(
    transport: &HttpJsonTransport,
    inputs: Value,
    original_request: &connect::Request,
    debug: &mut Option<ConnectorContext>,
) -> Result<http::Request<RouterBody>, HttpJsonTransportError> {
    let Value::Object(ref inputs_map) = inputs else {
        return Err(HttpJsonTransportError::InvalidArguments(
            "inputs must be a JSON object".to_string(),
        ));
    };
    let (json_body, body, apply_to_errors) = if let Some(ref selection) = transport.body {
        let inputs = inputs_map
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.clone()))
            .collect();
        let (json_body, apply_to_errors) = selection.apply_with_vars(&json!({}), &inputs);
        let body = if let Some(json_body) = json_body.as_ref() {
            hyper::Body::from(serde_json::to_vec(json_body)?)
        } else {
            hyper::Body::empty()
        };
        (json_body, body, apply_to_errors)
    } else {
        (None, hyper::Body::empty(), vec![])
    };

    let mut request = http::Request::builder()
        .method(transport.method.as_str())
        .uri(
            make_uri(transport, &inputs)
                .map_err(HttpJsonTransportError::ConnectorDirectiveError)?
                .as_str(),
        )
        .header("content-type", "application/json")
        .body(body.into())
        .map_err(HttpJsonTransportError::InvalidNewRequest)?;

    add_headers(
        &mut request,
        original_request.supergraph_request.headers(),
        &transport.headers,
    );

    if let Some(ref mut debug) = debug {
        debug.push_request(
            &request,
            json_body.as_ref(),
            transport.body.as_ref().map(|body| SelectionData {
                source: body.to_string(),
                transformed: body.to_string(),
                result: json_body.clone(),
                errors: apply_to_errors,
            }),
        );
    }

    Ok(request)
}

fn make_uri(transport: &HttpJsonTransport, inputs: &Value) -> Result<Url, ConnectorDirectiveError> {
    let flat_inputs = flatten_keys(inputs);
    let path = transport
        .path_template
        .generate_path(&Value::Object(flat_inputs))
        .map_err(ConnectorDirectiveError::PathGenerationError)?;
    append_path(Url::parse(transport.base_url.as_ref()).unwrap(), &path)
}

/// Append a path and query to a URI. Uses the path from base URI (but will discard the query).
/// Expects the path to start with "/".
fn append_path(base_uri: Url, path: &str) -> Result<Url, ConnectorDirectiveError> {
    // we will need to work on path segments, and on query parameters.
    // the first thing we need to do is parse the path so we have APIs to reason with both:
    let path_uri: Url = Url::options()
        .base_url(Some(&base_uri))
        .parse(path)
        .map_err(ConnectorDirectiveError::InvalidPath)?;
    // get query parameters from both base_uri and path
    let base_uri_query_pairs =
        (!base_uri.query().unwrap_or_default().is_empty()).then(|| base_uri.query_pairs());
    let path_uri_query_pairs =
        (!path_uri.query().unwrap_or_default().is_empty()).then(|| path_uri.query_pairs());

    let mut res = base_uri.clone();

    // append segments
    {
        // Path segments being none indicates the base_uri cannot be a base URL.
        // This means the schema is invalid.
        let base_segments = base_uri
            .path_segments()
            .ok_or(ConnectorDirectiveError::InvalidBaseUri(
                url::ParseError::RelativeUrlWithCannotBeABaseBase,
            ))?
            .filter(|segment| !segment.is_empty());

        let path_segments = path_uri
            .path_segments()
            .ok_or(ConnectorDirectiveError::InvalidPath(
                url::ParseError::RelativeUrlWithCannotBeABaseBase,
            ))?
            .filter(|segment| !segment.is_empty())
            // parsing encodes the segments, so we need to decode them before adding them
            .map(|segment| percent_decode_str(segment).decode_utf8().unwrap());

        // Ok this one is a bit tricky.
        // Here we're trying to only append segments that are not empty, to avoid `//`
        res.path_segments_mut()
            .map_err(|_| {
                ConnectorDirectiveError::InvalidBaseUri(
                    url::ParseError::RelativeUrlWithCannotBeABaseBase,
                )
            })?
            .clear()
            .extend(base_segments)
            .extend(path_segments);
    }
    // Calling clear on query_pairs will cause a `?` to be appended.
    // We only want to do it if necessary
    if base_uri_query_pairs.is_some() || path_uri_query_pairs.is_some() {
        res.query_pairs_mut().clear();
    }
    if let Some(pairs) = base_uri_query_pairs {
        res.query_pairs_mut().extend_pairs(pairs);
    }
    if let Some(pairs) = path_uri_query_pairs {
        res.query_pairs_mut().extend_pairs(pairs);
    }

    Ok(res)
}

// URLPathTemplate expects a map with flat dot-delimited keys.
fn flatten_keys(inputs: &Value) -> serde_json_bytes::Map<ByteString, Value> {
    let mut flat = serde_json_bytes::Map::new();
    flatten_keys_recursive(inputs, &mut flat, ByteString::from(""));
    flat
}

fn flatten_keys_recursive(
    inputs: &Value,
    flat: &mut serde_json_bytes::Map<ByteString, Value>,
    prefix: ByteString,
) {
    match inputs {
        Value::Object(map) => {
            for (key, value) in map {
                let mut new_prefix = prefix.as_str().to_string();
                if !new_prefix.is_empty() {
                    new_prefix += ".";
                }
                new_prefix += key.as_str();
                flatten_keys_recursive(value, flat, ByteString::from(new_prefix));
            }
        }
        _ => {
            flat.insert(prefix, inputs.clone());
        }
    }
}

fn add_headers<T>(
    request: &mut http::Request<T>,
    incoming_supergraph_headers: &HeaderMap<HeaderValue>,
    config: &[HTTPHeader],
) {
    let headers = request.headers_mut();
    for rule in config {
        match rule {
            HTTPHeader::Propagate { name } => {
                match HeaderName::from_str(name) {
                    Ok(name) => {
                        if RESERVED_HEADERS.contains(&name) {
                            tracing::warn!(
                                "Header '{}' is reserved and will not be propagated",
                                name
                            );
                        } else {
                            let values = incoming_supergraph_headers.get_all(&name);
                            let mut propagated = false;
                            for value in values {
                                headers.append(name.clone(), value.clone());
                                propagated = true;
                            }
                            if !propagated {
                                tracing::warn!("Header '{}' not found in incoming request", name);
                            }
                        }
                    }
                    Err(err) => {
                        tracing::error!("Invalid header name '{}': {:?}", name, err);
                    }
                };
            }
            HTTPHeader::Rename {
                original_name,
                new_name,
            } => match HeaderName::from_str(new_name) {
                Ok(new_name) => {
                    if RESERVED_HEADERS.contains(&new_name) {
                        tracing::warn!(
                            "Header '{}' is reserved and will not be propagated",
                            new_name
                        );
                    } else {
                        let values = incoming_supergraph_headers.get_all(original_name);
                        let mut propagated = false;
                        for value in values {
                            headers.append(new_name.clone(), value.clone());
                            propagated = true;
                        }
                        if !propagated {
                            tracing::warn!("Header '{}' not found in incoming request", new_name);
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Invalid header name '{}': {:?}", new_name, err);
                }
            },
            HTTPHeader::Inject { name, value } => match HeaderName::from_str(name) {
                Ok(name) => match HeaderValue::from_str(value) {
                    Ok(value) => {
                        headers.append(name, value);
                    }
                    Err(err) => {
                        tracing::error!("Invalid header value '{}': {:?}", value, err);
                    }
                },
                Err(err) => {
                    tracing::error!("Invalid header value '{}': {:?}", name, err);
                }
            },
        }
    }
}

// These are runtime error only, configuration errors should be captured as ConnectorDirectiveError
#[derive(Error, Display, Debug)]
pub(crate) enum HttpJsonTransportError {
    /// Error building URI: {0:?}
    NewUriError(#[from] Option<http::uri::InvalidUri>),
    /// Could not generate HTTP request: {0}
    InvalidNewRequest(#[source] http::Error),
    /// Could not serialize body: {0}
    BodySerialization(#[from] serde_json::Error),
    /// Invalid connector directive. This error should have been caught earlier: {0}
    ConnectorDirectiveError(#[source] ConnectorDirectiveError),
    /// Invalid arguments
    InvalidArguments(String),
}

#[cfg(test)]
mod tests {
    use apollo_federation::sources::connect::HTTPHeader;
    use http::header::CONTENT_ENCODING;
    use http::HeaderMap;
    use http::HeaderValue;

    use crate::plugins::connectors::http_json_transport::add_headers;

    #[test]
    fn append_path_test() {
        assert_eq!(
            super::append_path("https://localhost:8080/v1".parse().unwrap(), "/hello/42")
                .unwrap()
                .as_str(),
            "https://localhost:8080/v1/hello/42"
        );
    }

    #[test]
    fn append_path_test_with_trailing_slash() {
        assert_eq!(
            super::append_path("https://localhost:8080/".parse().unwrap(), "/hello/42")
                .unwrap()
                .as_str(),
            "https://localhost:8080/hello/42"
        );
    }

    #[test]
    fn append_path_test_with_trailing_slash_and_base_path() {
        assert_eq!(
            super::append_path("https://localhost:8080/v1/".parse().unwrap(), "/hello/42")
                .unwrap()
                .as_str(),
            "https://localhost:8080/v1/hello/42"
        );
    }
    #[test]
    fn append_path_test_with_and_base_path_and_params() {
        assert_eq!(
            super::append_path(
                "https://localhost:8080/v1?foo=bar".parse().unwrap(),
                "/hello/42"
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/hello/42?foo=bar"
        );
    }
    #[test]
    fn append_path_test_with_and_base_path_and_trailing_slash_and_params() {
        assert_eq!(
            super::append_path(
                "https://localhost:8080/v1/?foo=bar".parse().unwrap(),
                "/hello/42"
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/hello/42?foo=bar"
        );
    }

    #[test]
    fn append_path_test_with_encoded_characters() {
        assert_eq!(
            super::append_path(
                "https://localhost:8080/v1".parse().unwrap(),
                "/users/user%3A1" // must be authored encoded
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/users/user:1"
        );
    }

    #[test]
    fn test_headers_to_add_no_directives() {
        let incoming_supergraph_headers: HeaderMap<HeaderValue> = vec![
            (
                "x-propagate".parse().unwrap(),
                "propagated".parse().unwrap(),
            ),
            ("x-rename".parse().unwrap(), "renamed".parse().unwrap()),
            ("x-ignore".parse().unwrap(), "ignored".parse().unwrap()),
            (CONTENT_ENCODING, "gzip".parse().unwrap()),
        ]
        .into_iter()
        .collect();

        let mut request = http::Request::builder().body(hyper::Body::empty()).unwrap();
        add_headers(&mut request, &incoming_supergraph_headers, &[]);
        assert!(request.headers().is_empty());
    }

    #[test]
    fn test_headers_to_add_with_config() {
        let incoming_supergraph_headers: HeaderMap<HeaderValue> = vec![
            (
                "x-propagate".parse().unwrap(),
                "propagated".parse().unwrap(),
            ),
            ("x-rename".parse().unwrap(), "renamed".parse().unwrap()),
            ("x-ignore".parse().unwrap(), "ignored".parse().unwrap()),
            (CONTENT_ENCODING, "gzip".parse().unwrap()),
        ]
        .into_iter()
        .collect();

        let config = vec![
            HTTPHeader::Propagate {
                name: "x-propagate".parse().unwrap(),
            },
            HTTPHeader::Rename {
                original_name: "x-rename".parse().unwrap(),
                new_name: "x-new-name".parse().unwrap(),
            },
            HTTPHeader::Inject {
                name: "x-insert".parse().unwrap(),
                value: "inserted".parse().unwrap(),
            },
        ];

        let mut request = http::Request::builder().body(hyper::Body::empty()).unwrap();
        add_headers(&mut request, &incoming_supergraph_headers, &config);
        let result = request.headers();
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("x-new-name"), Some(&"renamed".parse().unwrap()));
        assert_eq!(result.get("x-insert"), Some(&"inserted".parse().unwrap()));
        assert_eq!(
            result.get("x-propagate"),
            Some(&"propagated".parse().unwrap())
        );
    }

    #[test]
    fn test_flatten_keys() {
        let inputs = serde_json_bytes::json!({
            "a": 1,
            "b": {
                "c": 2,
                "d": {
                    "e": 3
                }
            }
        });
        let flat = super::flatten_keys(&inputs);
        assert_eq!(
            flat,
            serde_json_bytes::json!({
                "a": 1,
                "b.c": 2,
                "b.d.e": 3
            })
            .as_object()
            .unwrap()
            .clone()
        );
    }
}
