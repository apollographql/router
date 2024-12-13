use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_federation::sources::connect::HTTPMethod;
use apollo_federation::sources::connect::HeaderSource;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::connect::URLTemplate;
use displaydoc::Display;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use parking_lot::Mutex;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use thiserror::Error;
use url::Url;

use super::form_encoding::encode_json_as_form;
use crate::plugins::connectors::plugin::debug::serialize_request;
use crate::plugins::connectors::plugin::debug::ConnectorContext;
use crate::plugins::connectors::plugin::debug::ConnectorDebugHttpRequest;
use crate::plugins::connectors::plugin::debug::SelectionData;
use crate::services::connect;
use crate::services::router::body::RouterBody;

pub(crate) fn make_request(
    transport: &HttpJsonTransport,
    inputs: IndexMap<String, Value>,
    original_request: &connect::Request,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<(http::Request<RouterBody>, Option<ConnectorDebugHttpRequest>), HttpJsonTransportError>
{
    let uri = make_uri(
        transport.source_url.as_ref(),
        &transport.connect_template,
        &inputs,
    )?;

    let request = http::Request::builder()
        .method(transport.method.as_str())
        .uri(uri.as_str());

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
                    (hyper::Body::from(encoded), len)
                } else {
                    request = request.header(CONTENT_TYPE, mime::APPLICATION_JSON.essence_str());
                    let bytes = serde_json::to_vec(json_body)?;
                    let len = bytes.len();
                    (hyper::Body::from(bytes), len)
                }
            } else {
                (hyper::Body::empty(), 0)
            };
            (json_body, form_body, body, content_length, apply_to_errors)
        } else {
            (None, None, hyper::Body::empty(), 0, vec![])
        };

    match transport.method {
        HTTPMethod::Post | HTTPMethod::Patch | HTTPMethod::Put => {
            request = request.header(CONTENT_LENGTH, content_length);
        }
        _ => {}
    }

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
    inputs: &IndexMap<String, Value>,
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
                .map_err(|err| HttpJsonTransportError::TemplateGenerationError(err.message))?,
        );

    let query_params = template
        .interpolate_query(inputs)
        .map_err(|err| HttpJsonTransportError::TemplateGenerationError(err.message))?;
    if !query_params.is_empty() {
        url.query_pairs_mut().extend_pairs(query_params);
    }
    Ok(url)
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
    NewUriError(#[from] Option<http::uri::InvalidUri>),
    /// Could not generate HTTP request: {0}
    InvalidNewRequest(#[source] http::Error),
    /// Could not serialize body: {0}
    JsonBodySerialization(#[from] serde_json::Error),
    /// Could not serialize body: {0}
    FormBodySerialization(&'static str),
    /// Error building URI: {0:?}
    InvalidUrl(url::ParseError),
    /// Could not generate URI from inputs: {0}
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

    macro_rules! this {
        ($($value:tt)*) => {{
            let mut map = IndexMap::with_capacity_and_hasher(1, Default::default());
            map.insert("$this".to_string(), json!({ $($value)* }));
            map
        }};
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
                &this! { "id": 42 },
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
                &this! {"id": 42 },
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
                &this! {"id": 42 },
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
                .unwrap()
                .as_str(),
            @"http://localhost/users/?a=&e="
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "user_id": 123,
                    "b": "456",
                    "f": {"g": "abc"}
                }
            )
            .unwrap()
            .to_string(),
            @"http://localhost/users/123?a=456&e=abc"
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "user_id": 123,
                    "f": "not an object"
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/users/123?a=&e="
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    // The order of the variables should not matter.
                    "b": "456",
                    "user_id": "123"
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/users/123?a=456&e="
        );

        assert_eq!(
            make_uri(
                None,
                &template,
                &this! {
                    "user_id": "123",
                    "b": "a",
                    "f": {"g": "e"},
                    // Extra variables should be ignored.
                    "extra": "ignored"
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
                &this! {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                    "b": 4,
                    "c": 5,
                    "d": 6,
                    "e": 7,
                    "f": 8,
                }
            )
            .unwrap()
            .as_str(),
            "http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B7%2C8%5D"
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                    "b": 4,
                    "c": 5,
                    "d": 6,
                    "e": 7
                    // "f": 8,
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B7%2C%5D",
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                    "b": 4,
                    "c": 5,
                    "d": 6,
                    // "e": 7,
                    "f": 8
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B%2C8%5D",
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                    "b": 4,
                    "c": 5,
                    "d": 6
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B%2C%5D",
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    // "x": 1,
                    "y": 2,
                    "z": 3
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/locations/xyz(,2,3)?required=%2C%3B&optional=%5B%2C%5D",
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "x": 1,
                    "y": 2
                    // "z": 3,
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/locations/xyz(1,2,)?required=%2C%3B&optional=%5B%2C%5D"
        );

        assert_snapshot!(
            make_uri(
                None,
                &template,
                &this! {
                    "b": 4,
                    // "c": 5,
                    "d": 6,
                    "x": 1,
                    "y": 2,
                    "z": 3
                }
            )
            .unwrap()
            .to_string(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C%3B6&optional=%5B%2C%5D"
        );

        let line_template = "http://localhost/line/{$this.p1.x},{$this.p1.y},{$this.p1.z}/{$this.p2.x},{$this.p2.y},{$this.p2.z}"
            .parse()
            .unwrap();

        assert_snapshot!(
            make_uri(
                None,
                &line_template,
                &this! {
                    "p1": {
                        "x": 1,
                        "y": 2,
                        "z": 3,
                    },
                    "p2": {
                        "x": 4,
                        "y": 5,
                        "z": 6,
                    }
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/line/1,2,3/4,5,6"
        );

        assert_snapshot!(
            make_uri(
                None,
                &line_template,
            &this! {
                "p1": {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                },
                "p2": {
                    "x": 4,
                    "y": 5,
                    // "z": 6,
                }
            }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/line/1,2,3/4,5,"
        );

        assert_snapshot!(
            make_uri(
                None,
                &line_template,
                &this! {
                    "p1": {
                        "x": 1,
                        // "y": 2,
                        "z": 3,
                    },
                    "p2": {
                        "x": 4,
                        "y": 5,
                        "z": 6,
                    }
                }
            )
            .unwrap()
            .as_str(),
            @"http://localhost/line/1,,3/4,5,6"
        );
    }

    /// Values are all strings, they can't have semantic value for HTTP. That means no dynamic paths,
    /// no nested query params, etc. When we expand values, we have to make sure they're safe.
    #[test]
    fn parameter_encoding() {
        let vars = &this! {
            "path": "/some/path",
            "question_mark": "a?b",
            "ampersand": "a&b=b",
            "hash": "a#b",
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
    use std::str::FromStr;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HeaderSource;
    use apollo_federation::sources::connect::JSONSelection;
    use http::header::CONTENT_ENCODING;
    use http::HeaderMap;
    use http::HeaderValue;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::Context;

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
            &IndexMap::with_hasher(Default::default()),
        );
        let request = request.body(hyper::Body::empty()).unwrap();
        let result = request.headers();
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("x-new-name"), Some(&"renamed".parse().unwrap()));
        assert_eq!(result.get("x-insert"), Some(&"inserted".parse().unwrap()));
    }

    #[test]
    fn make_request() {
        let schema = Schema::parse_and_validate("type Query { f(a: Int): String }", "").unwrap();
        let doc = ExecutableDocument::parse_and_validate(&schema, "{f(a: 42)}", "").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "a": 42 }));

        let req = super::make_request(
            &HttpJsonTransport {
                source_url: None,
                connect_template: URLTemplate::from_str("http://localhost:8080/").unwrap(),
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
            },
            &None,
        )
        .unwrap();

        assert_debug_snapshot!(req, @r###"
        (
            Request {
                method: POST,
                uri: http://localhost:8080/,
                version: HTTP/1.1,
                headers: {
                    "content-type": "application/json",
                    "content-length": "8",
                },
                body: Body(
                    Full(
                        b"{\"a\":42}",
                    ),
                ),
            },
            None,
        )
        "###);
    }

    #[test]
    fn make_request_form_encoded() {
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
                connect_template: URLTemplate::from_str("http://localhost:8080/").unwrap(),
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
            },
            &None,
        )
        .unwrap();

        assert_debug_snapshot!(req, @r###"
        (
            Request {
                method: POST,
                uri: http://localhost:8080/,
                version: HTTP/1.1,
                headers: {
                    "content-type": "application/x-www-form-urlencoded",
                    "content-length": "4",
                },
                body: Body(
                    Full(
                        b"a=42",
                    ),
                ),
            },
            None,
        )
        "###);
    }
}
