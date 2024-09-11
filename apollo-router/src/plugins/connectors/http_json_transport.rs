use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_federation::sources::connect::HeaderSource;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::connect::URLTemplate;
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
use parking_lot::Mutex;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use thiserror::Error;
use url::Url;

use super::form_encoding::encode_json_as_form;
use super::plugin::serialize_request;
use super::plugin::ConnectorDebugHttpRequest;
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
    inputs: IndexMap<String, Value>,
    original_request: &connect::Request,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<(http::Request<RouterBody>, Option<ConnectorDebugHttpRequest>), HttpJsonTransportError>
{
    let flat_inputs = flatten_keys(&inputs);
    let uri = make_uri(
        transport.source_url.as_ref(),
        &transport.connect_template,
        &flat_inputs,
    )?;

    let request = http::Request::builder()
        .method(transport.method.as_str())
        .uri(uri.as_str());

    // add the headers and if content-type is specified, we'll check that when constructing the body
    let (mut request, content_type) = add_headers(
        request,
        original_request.supergraph_request.headers(),
        &transport.headers,
        &flat_inputs,
    );

    let is_form_urlencoded = content_type.as_ref() == Some(&mime::APPLICATION_WWW_FORM_URLENCODED);

    let (json_body, form_body, body, apply_to_errors) = if let Some(ref selection) = transport.body
    {
        let (json_body, apply_to_errors) = selection.apply_with_vars(&json!({}), &inputs);
        let mut form_body = None;
        let body = if let Some(json_body) = json_body.as_ref() {
            if is_form_urlencoded {
                let encoded = encode_json_as_form(json_body)
                    .map_err(HttpJsonTransportError::FormBodySerialization)?;
                form_body = Some(encoded.clone());
                hyper::Body::from(encoded)
            } else {
                request = request.header(CONTENT_TYPE, mime::APPLICATION_JSON.essence_str());
                hyper::Body::from(serde_json::to_vec(json_body)?)
            }
        } else {
            hyper::Body::empty()
        };
        (json_body, form_body, body, apply_to_errors)
    } else {
        (None, None, hyper::Body::empty(), vec![])
    };

    let request = request
        .body(body.into())
        .map_err(HttpJsonTransportError::InvalidNewRequest)?;

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
                    errors: apply_to_errors,
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
                    errors: apply_to_errors,
                }),
            )
        }
    });

    Ok((request, debug_request))
}

fn make_uri(
    source_url: Option<&Url>,
    template: &URLTemplate,
    inputs: &Map<ByteString, Value>,
) -> Result<Url, HttpJsonTransportError> {
    let mut url = source_url
        .or(template.base.as_ref())
        .ok_or(HttpJsonTransportError::NoBaseUrl)?
        .clone();

    url.path_segments_mut()
        .map_err(|_| {
            HttpJsonTransportError::InvalidUrl(url::ParseError::RelativeUrlWithCannotBeABaseBase)
        })?
        .pop_if_empty()
        .extend(
            template
                .interpolate_path(inputs)
                .map_err(HttpJsonTransportError::TemplateGenerationError)?,
        );

    let query_params = template.interpolate_query(inputs);
    if !query_params.is_empty() {
        url.query_pairs_mut().extend_pairs(query_params);
    }
    Ok(url)
}

// URLTemplate expects a map with flat dot-delimited keys.
fn flatten_keys(inputs: &IndexMap<String, Value>) -> Map<ByteString, Value> {
    let mut flat = serde_json_bytes::Map::with_capacity(inputs.len());
    for (key, value) in inputs {
        flatten_keys_recursive(value, &mut flat, key.clone());
    }
    flat
}

fn flatten_keys_recursive(inputs: &Value, flat: &mut Map<ByteString, Value>, prefix: String) {
    match inputs {
        Value::Object(map) => {
            for (key, value) in map {
                flatten_keys_recursive(value, flat, [prefix.as_str(), ".", key.as_str()].concat());
            }
        }
        _ => {
            flat.insert(prefix, inputs.clone());
        }
    }
}

#[allow(clippy::mutable_key_type)] // HeaderName is internally mutable, but safe to use in maps
fn add_headers(
    mut request: http::request::Builder,
    incoming_supergraph_headers: &HeaderMap<HeaderValue>,
    config: &IndexMap<HeaderName, HeaderSource>,
    inputs: &Map<ByteString, Value>,
) -> (http::request::Builder, Option<mime::Mime>) {
    let mut content_type = None;

    for (header_name, header_source) in config {
        match header_source {
            HeaderSource::From(from) => {
                if RESERVED_HEADERS.contains(&header_name) {
                    tracing::warn!(
                        "Header '{}' is reserved and will not be propagated",
                        header_name
                    );
                } else {
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
            }
            HeaderSource::Value(value) => match value.interpolate(inputs) {
                Ok(value) => match HeaderValue::from_str(value.as_str()) {
                    Ok(value) => {
                        request = request.header(header_name, value.clone());

                        if header_name == CONTENT_TYPE {
                            content_type = Some(value.clone());
                        }
                    }
                    Err(err) => {
                        tracing::error!("Invalid header value '{:?}': {:?}", value, err);
                    }
                },
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
    NewUriError(#[from] Option<http::uri::InvalidUri>),
    /// Could not generate HTTP request: {0}
    InvalidNewRequest(#[source] http::Error),
    /// Could not serialize body: {0}
    JsonBodySerialization(#[from] serde_json::Error),
    /// Could not serialize body: {0}
    FormBodySerialization(&'static str),
    /// Error building URI: {0:?}
    InvalidUrl(url::ParseError),
    /// Could not generate path from inputs: {0}
    TemplateGenerationError(String),
    /// Either a source or a fully qualified URL must be provided to `@connect`
    NoBaseUrl,
}

#[cfg(test)]
mod test_make_uri {
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::json;

    use super::*;

    macro_rules! map {
        ($($key:expr => $value:expr),* $(,)?) => {
            {
                let mut variables = IndexMap::with_hasher(Default::default());
                let mut this = IndexMap::with_hasher(Default::default());
                $(
                    this.insert($key.to_string(), json!($value));
                )*
                variables.insert("$this".to_string(), json!(this));
                flatten_keys(&variables)
            }
        };
    }

    #[test]
    fn append_path() {
        assert_eq!(
            make_uri(
                Some(&Url::parse("https://localhost:8080/v1").unwrap()),
                &"/hello/42".parse().unwrap(),
                &Default::default(),
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/hello/42"
        );
    }

    #[test]
    fn append_path_with_trailing_slash() {
        assert_eq!(
            make_uri(
                Some(&Url::parse("https://localhost:8080/").unwrap()),
                &"/hello/42".parse().unwrap(),
                &Default::default(),
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/hello/42"
        );
    }

    #[test]
    fn append_path_test_with_trailing_slash_and_base_path() {
        assert_eq!(
            make_uri(
                Some(&Url::parse("https://localhost:8080/v1/").unwrap()),
                &"/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                &map! { "id" => 42 },
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/hello/42?id=42"
        );
    }
    #[test]
    fn append_path_test_with_and_base_path_and_params() {
        assert_eq!(
            make_uri(
                Some(&Url::parse("https://localhost:8080/v1?foo=bar").unwrap()),
                &"/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                &map! {"id" => 42 },
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/hello/42?foo=bar&id=42"
        );
    }
    #[test]
    fn append_path_test_with_and_base_path_and_trailing_slash_and_params() {
        assert_eq!(
            make_uri(
                Some(&Url::parse("https://localhost:8080/v1/?foo=bar").unwrap()),
                &"/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                &map! {"id" => 42 },
            )
            .unwrap()
            .as_str(),
            "https://localhost:8080/v1/hello/42?foo=bar&id=42"
        );
    }

    #[test]
    fn path_cases() {
        let template = "http://localhost/users/{$this.user_id}?a={$this.b}&e={$this.f.g}"
            .parse()
            .unwrap();

        assert_snapshot!(
            make_uri(None, &template, &Default::default())
                .err()
                .unwrap()
                .to_string(),
            @"Could not generate path from inputs: Path parameter {$this.user_id} was missing one or more values in {}"
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "user_id" => 123,
                    "b" => "456",
                    "f.g" => "abc"
                }
            )
            .unwrap()
            .to_string(),
            "http://localhost/users/123?a=456&e=abc"
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "user_id" => 123,
                    "f" => "not an object"
                }
            )
            .unwrap()
            .to_string(),
            "http://localhost/users/123"
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    // The order of the variables should not matter.
                    "b" => "456",
                    "user_id" => "123"
                }
            )
            .unwrap()
            .to_string(),
            "http://localhost/users/123?a=456"
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "user_id" => "123",
                    "b" => "a",
                    "f.g" => "e",
                    // Extra variables should be ignored.
                    "extra" => "ignored"
                }
            )
            .unwrap()
            .to_string(),
            "http://localhost/users/123?a=a&e=e",
        );
    }

    #[test]
    fn multi_variable_parameter_values() {
        let template =
            "http://localhost/locations/xyz({$this.x},{$this.y},{$this.z})?required={$this.b},{$this.c};{$this.d}&optional=[{$this.e},{$this.f}]"
                .parse()
                .unwrap();

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "x" => 1,
                    "y" => 2,
                    "z" => 3,
                    "b" => 4,
                    "c" => 5,
                    "d" => 6,
                    "e" => 7,
                    "f" => 8,
                }
            )
            .unwrap()
            .as_str(),
            "http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B7%2C8%5D"
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "x" => 1,
                    "y" => 2,
                    "z" => 3,
                    "b" => 4,
                    "c" => 5,
                    "d" => 6,
                    "e" => 7
                    // "f" => 8,
                }
            )
            .unwrap()
            .as_str(),
            "http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6",
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "x" => 1,
                    "y" => 2,
                    "z" => 3,
                    "b" => 4,
                    "c" => 5,
                    "d" => 6,
                    // "e" => 7,
                    "f" => 8
                }
            )
            .unwrap()
            .as_str(),
            "http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6",
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &map! {
                    "x" => 1,
                    "y" => 2,
                    "z" => 3,
                    "b" => 4,
                    "c" => 5,
                    "d" => 6
                }
            )
            .unwrap()
            .as_str(),
            "http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6",
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &map! {
                    // "x" => 1,
                    "y" => 2,
                    "z" => 3
                }
            )
            .err()
            .unwrap()
            .to_string(),
            @r###"Could not generate path from inputs: Path parameter xyz({$this.x},{$this.y},{$this.z}) was missing one or more values in {"$this.y": Number(2), "$this.z": Number(3)}"###,
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &map! {
                    "x" => 1,
                    "y" => 2
                    // "z" => 3,
                }
            )
            .err()
            .unwrap()
            .to_string(),
            @r###"Could not generate path from inputs: Path parameter xyz({$this.x},{$this.y},{$this.z}) was missing one or more values in {"$this.x": Number(1), "$this.y": Number(2)}"###
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &map! {
                    "b" => 4,
                    // "c" => 5,
                    "d" => 6,
                    "x" => 1,
                    "y" => 2,
                    "z" => 3
                }
            )
            .unwrap()
            .to_string(),
            @"http://localhost/locations/xyz(1,2,3)"
        );

        let line_template = "http://localhost/line/{$this.p1.x},{$this.p1.y},{$this.p1.z}/{$this.p2.x},{$this.p2.y},{$this.p2.z}"
            .parse()
            .unwrap();

        assert_eq!(
            make_uri(
                None,
                &line_template,
                &map! {
                    "p1.x" => 1,
                    "p1.y" => 2,
                    "p1.z" => 3,
                    "p2.x" => 4,
                    "p2.y" => 5,
                    "p2.z" => 6,
                }
            )
            .unwrap()
            .as_str(),
            "http://localhost/line/1,2,3/4,5,6"
        );

        assert_snapshot!(
            make_uri(
                None,
                &line_template,
                &map! {
                    "p1.x" => 1,
                    "p1.y" => 2,
                    "p1.z" => 3,
                    "p2.x" => 4,
                    "p2.y" => 5
                    // "p2.z" => 6,
                }
            )
            .err()
            .unwrap()
            .to_string(),
            @r###"Could not generate path from inputs: Path parameter {$this.p2.x},{$this.p2.y},{$this.p2.z} was missing one or more values in {"$this.p1.x": Number(1), "$this.p1.y": Number(2), "$this.p1.z": Number(3), "$this.p2.x": Number(4), "$this.p2.y": Number(5)}"###
        );

        assert_snapshot!(
            make_uri(
                None,
                &line_template,
                &map! {
                    "p1.x" => 1,
                    // "p1.y" => 2,
                    "p1.z" => 3,
                    "p2.x" => 4,
                    "p2.y" => 5,
                    "p2.z" => 6
                }
            )
            .err()
            .unwrap()
            .to_string(),
            @r###"Could not generate path from inputs: Path parameter {$this.p1.x},{$this.p1.y},{$this.p1.z} was missing one or more values in {"$this.p1.x": Number(1), "$this.p1.z": Number(3), "$this.p2.x": Number(4), "$this.p2.y": Number(5), "$this.p2.z": Number(6)}"###
        );
    }

    /// Values are all strings, they can't have semantic value for HTTP. That means no dynamic paths,
    /// no nested query params, etc. When we expand values, we have to make sure they're safe.
    #[test]
    fn parameter_encoding() {
        let vars = &map! {
            "path" => "/some/path",
            "question_mark" => "a?b",
            "ampersand" => "a&b=b",
            "hash" => "a#b",
        };

        let template = "http://localhost/{$this.path}/{$this.question_mark}?a={$this.ampersand}&c={$this.hash}"
            .parse()
            .expect("Failed to parse URL template");
        let url = make_uri(None, &template, vars).expect("Failed to generate URL");

        assert_eq!(
            url.as_str(),
            "http://localhost/%2Fsome%2Fpath/a%3Fb?a=a%26b%3Db&c=a%23b"
        );
    }
}

#[cfg(test)]
mod tests {
    use apollo_federation::sources::connect::HeaderSource;
    use http::header::CONTENT_ENCODING;
    use http::HeaderMap;
    use http::HeaderValue;

    use super::*;

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
            &Map::default(),
        );
        let request = request.body(hyper::Body::empty()).unwrap();
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
            &Map::default(),
        );
        let request = request.body(hyper::Body::empty()).unwrap();
        let result = request.headers();
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("x-new-name"), Some(&"renamed".parse().unwrap()));
        assert_eq!(result.get("x-insert"), Some(&"inserted".parse().unwrap()));
    }

    #[test]
    fn test_flatten_keys() {
        let mut inputs = IndexMap::with_hasher(Default::default());
        inputs.insert("a".to_string(), json!(1));
        inputs.insert(
            "b".to_string(),
            json!({
                    "c": 2,
                    "d": {
                        "e": 3
                    }
            }),
        );
        let flat = flatten_keys(&inputs);
        assert_eq!(
            flat,
            json!({
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
