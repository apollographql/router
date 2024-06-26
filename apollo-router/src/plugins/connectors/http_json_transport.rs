use std::collections::HashSet;
use std::sync::Arc;

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
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use thiserror::Error;
use url::Url;

use crate::error::ConnectorDirectiveError;
use crate::services::router::body::RouterBody;

// Copied from plugins::headers
// Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
// These are not propagated by default using a regex match as they will not make sense for the
// second hop.
// In addition because our requests are not regular proxy requests content-type, content-length
// and host are also in the exclude list.
#[allow(dead_code)]
static RESERVED_HEADERS: [HeaderName; 14] = [
    CONNECTION,
    PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION,
    TE,
    TRAILER,
    TRANSFER_ENCODING,
    UPGRADE,
    CONTENT_LENGTH,
    CONTENT_TYPE,
    CONTENT_ENCODING,
    HOST,
    ACCEPT,
    ACCEPT_ENCODING,
    HeaderName::from_static("keep-alive"),
];

pub(crate) fn make_request(
    transport: &HttpJsonTransport,
    inputs: Value,
) -> Result<http::Request<RouterBody>, HttpJsonTransportError> {
    let body = hyper::Body::empty();

    // TODO: why is apollo_federation HTTPMethod Ucfirst?
    let method = transport.method.to_string().to_uppercase();
    let request = http::Request::builder()
        .method(method.as_bytes())
        .uri(
            make_uri(transport, &inputs)
                .map_err(HttpJsonTransportError::ConnectorDirectiveError)?
                .as_str(),
        )
        .header("content-type", "application/json")
        .body(body.into())
        .map_err(HttpJsonTransportError::InvalidNewRequest)?;

    Ok(request)
}

fn make_uri(
    transport: &HttpJsonTransport,
    inputs: &Value,
) -> Result<url::Url, ConnectorDirectiveError> {
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
        let segments = base_uri
            .path_segments()
            .ok_or(ConnectorDirectiveError::InvalidBaseUri(
                url::ParseError::RelativeUrlWithCannotBeABaseBase,
            ))?;

        // Ok this one is a bit tricky.
        // Here we're trying to only append segments that are not empty, to avoid `//`
        let mut res_segments = res.path_segments_mut().map_err(|_| {
            ConnectorDirectiveError::InvalidBaseUri(
                url::ParseError::RelativeUrlWithCannotBeABaseBase,
            )
        })?;
        res_segments
            .clear()
            .extend(segments.filter(|segment| !segment.is_empty()))
            .extend(
                path_uri
                    .path_segments()
                    .ok_or(ConnectorDirectiveError::InvalidPath(
                        url::ParseError::RelativeUrlWithCannotBeABaseBase,
                    ))?
                    .filter(|segment| !segment.is_empty()),
            );
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

#[allow(dead_code)]
fn headers_to_add(
    incoming_supergraph_headers: &HeaderMap<HeaderValue>,
    incoming_subgraph_headers: &HeaderMap<HeaderValue>,
    reserved_headers: Arc<HashSet<&'static HeaderName>>,
    config: &[HttpHeader],
) -> Vec<(HeaderName, HeaderValue)> {
    if config.is_empty() {
        incoming_supergraph_headers
            .iter()
            .chain(incoming_subgraph_headers.iter())
            .filter(|(name, _)| !reserved_headers.contains(name))
            .map(|(n, v)| (n.clone(), v.clone()))
            .collect()
    } else {
        config
            .iter()
            .flat_map(|rule| match rule {
                HttpHeader::Propagate { name } => {
                    #[allow(clippy::manual_map)]
                    match incoming_supergraph_headers
                        .get(name)
                        .or(incoming_subgraph_headers.get(name))
                    {
                        Some(value) => Some((name.clone(), value.clone())),
                        None => None, // TODO log?
                    }
                }

                HttpHeader::Rename {
                    original_name,
                    new_name,
                } => {
                    #[allow(clippy::manual_map)]
                    match incoming_supergraph_headers
                        .get(original_name)
                        .or(incoming_subgraph_headers.get(original_name))
                    {
                        Some(value) => Some((new_name.clone(), value.clone())),
                        None => None, // TODO log?
                    }
                }

                HttpHeader::Inject { name, value } => Some((name.clone(), value.clone())),
            })
            .filter(|(name, _)| !reserved_headers.contains(name))
            .collect()
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(super) enum HttpHeader {
    Propagate {
        name: HeaderName,
    },
    Rename {
        original_name: HeaderName,
        new_name: HeaderName,
    },
    Inject {
        name: HeaderName,
        value: HeaderValue,
    },
}

// These are runtime error only, configuration errors should be captured as ConnectorDirectiveError
#[derive(Error, Display, Debug)]
#[allow(dead_code)]
pub(crate) enum HttpJsonTransportError {
    /// Error building URI: {0:?}
    NewUriError(#[from] Option<http::uri::InvalidUri>),
    /// Could not generate HTTP request: {0}
    InvalidNewRequest(#[source] http::Error),
    /// Could not serialize body: {0}
    BodySerialization(#[source] serde_json::Error),
    /// Invalid connector directive. This error should have been caught earlier: {0}
    ConnectorDirectiveError(#[source] ConnectorDirectiveError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::header::CONTENT_ENCODING;
    use http::HeaderMap;
    use http::HeaderValue;

    use super::headers_to_add;
    use super::HttpHeader;
    use super::RESERVED_HEADERS;

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

        let incoming_subgraph_headers: HeaderMap<HeaderValue> = vec![].into_iter().collect();

        let results = headers_to_add(
            &incoming_supergraph_headers,
            &incoming_subgraph_headers,
            Arc::new(RESERVED_HEADERS.iter().collect()),
            &[],
        );
        assert_eq!(
            results,
            vec![
                (
                    "x-propagate".parse().unwrap(),
                    "propagated".parse().unwrap(),
                ),
                ("x-rename".parse().unwrap(), "renamed".parse().unwrap()),
                ("x-ignore".parse().unwrap(), "ignored".parse().unwrap()),
            ]
        );
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

        let incoming_subgraph_headers: HeaderMap<HeaderValue> = vec![].into_iter().collect();

        let config = vec![
            HttpHeader::Propagate {
                name: "x-propagate".parse().unwrap(),
            },
            HttpHeader::Rename {
                original_name: "x-rename".parse().unwrap(),
                new_name: "x-new-name".parse().unwrap(),
            },
            HttpHeader::Inject {
                name: "x-insert".parse().unwrap(),
                value: "inserted".parse().unwrap(),
            },
        ];

        let results = headers_to_add(
            &incoming_supergraph_headers,
            &incoming_subgraph_headers,
            Arc::new(RESERVED_HEADERS.iter().collect()),
            &config,
        );
        assert_eq!(
            results,
            vec![
                (
                    "x-propagate".parse().unwrap(),
                    "propagated".parse().unwrap(),
                ),
                ("x-new-name".parse().unwrap(), "renamed".parse().unwrap()),
                ("x-insert".parse().unwrap(), "inserted".parse().unwrap()),
            ]
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
