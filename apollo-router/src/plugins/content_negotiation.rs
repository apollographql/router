//! The content negotiation plugin performs HTTP content negotiation using the `accept` and
//! `content-type` headers, working at the router stage.
use std::ops::ControlFlow;

use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::header::VARY;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::protocols::multipart::ProtocolMode;
use crate::services::router;
use crate::services::router::body::RouterBody;

register_plugin!("apollo", "content_negotiation", ContentNegotiation);

const APPLICATION_JSON: &str = "application/json";
pub(crate) const APPLICATION_GRAPHQL_JSON: &str = "application/graphql-response+json";

const ORIGIN_HEADER_VALUE: HeaderValue = HeaderValue::from_static("origin");

// set the supported `@defer` specification version to https://github.com/graphql/graphql-spec/pull/742/commits/01d7b98f04810c9a9db4c0e53d3c4d54dbf10b82
const MULTIPART_DEFER_SPEC_PARAMETER: &str = "deferSpec";
const MULTIPART_DEFER_SPEC_VALUE: &str = "20220824";
pub(crate) const MULTIPART_DEFER_ACCEPT_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("multipart/mixed;deferSpec=20220824");
pub(crate) const MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("multipart/mixed;boundary=\"graphql\";deferSpec=20220824");

const MULTIPART_SUBSCRIPTION_ACCEPT: &str = "multipart/mixed;subscriptionSpec=1.0";
const MULTIPART_SUBSCRIPTION_SPEC_PARAMETER: &str = "subscriptionSpec";
const MULTIPART_SUBSCRIPTION_SPEC_VALUE: &str = "1.0";
pub(crate) const MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("multipart/mixed;boundary=\"graphql\";subscriptionSpec=1.0");

/// The `ClientRequestAccepts` struct is effectively a parsed version of a request's `accept` header.
///
/// Note that multiple values here can be set to true. For example, if the request provides
/// header value `application/json,*/*`, both `json` and `wildcard` in the struct will be set to true.
#[derive(Clone, Default, Debug)]
pub(crate) struct ClientRequestAccepts {
    pub(crate) multipart_defer: bool,
    pub(crate) multipart_subscription: bool,
    pub(crate) json: bool,
    pub(crate) wildcard: bool,
}

impl ClientRequestAccepts {
    /// Returns true if any of the struct's members are true, ie the request includes an `accept`
    /// value that the router supports.
    fn is_valid(&self) -> bool {
        self.json || self.wildcard || self.multipart_defer || self.multipart_subscription
    }
}

/// The `ContentNegotiation` plugin provides request and response layers at the router service.
///
/// # Request
/// The request layer rejects requests that do not have an expected `Content-Type`, or that have an
/// `Accept` header that is not supported by the router.
///
/// In particular:
///   * the request must be a `GET` or have `CONTENT_TYPE = JSON`, and
///   * the accept header must include `*/*`, one of the JSON/GraphQL MIME types, or one of the
///     multipart types.
///
/// It will also add a `ClientRequestAccepts` value to the context if the request is valid.
///
/// # Response
/// The response layer sets the `CONTENT_TYPE` header, using the `ClientRequestAccepts` value from
/// the context (set on the request side of this plugin). It will also set the `VARY` header if it
/// is not present.
///
/// # Context
/// If the request is valid, this layer adds a [`ClientRequestAccepts`] value to the context.
struct ContentNegotiation {}
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Config {}

impl ContentNegotiation {
    fn handle_request(request: router::Request) -> ControlFlow<router::Response, router::Request> {
        let valid_content_type_header = request.router_request.method() == Method::GET
            || content_type_includes_json(request.router_request.headers());
        if !valid_content_type_header {
            return ControlFlow::Break(invalid_content_type_header_response().into());
        }

        let accepts = parse_accept_header(request.router_request.headers());
        if !accepts.is_valid() {
            return ControlFlow::Break(invalid_accept_header_response().into());
        }

        request
            .context
            .extensions()
            .with_lock(|lock| lock.insert(accepts));
        ControlFlow::Continue(request)
    }

    fn handle_response(mut response: router::Response) -> router::Response {
        let ClientRequestAccepts {
            multipart_defer: accepts_multipart_defer,
            multipart_subscription: accepts_multipart_subscription,
            json: accepts_json,
            wildcard: accepts_wildcard,
        } = response.context.extensions().with_lock(|lock| {
            lock.get::<ClientRequestAccepts>()
                .cloned()
                .unwrap_or_default()
        });

        let headers = response.response.headers_mut();
        process_vary_header(headers);

        let protocol_mode = response
            .context
            .extensions()
            .with_lock(|lock| lock.get::<ProtocolMode>().cloned());

        let content_type = match protocol_mode {
            Some(ProtocolMode::Defer) if accepts_multipart_defer => {
                MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE
            }
            Some(ProtocolMode::Subscription) if accepts_multipart_subscription => {
                MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE
            }
            None if accepts_json || accepts_wildcard => HeaderValue::from_static(APPLICATION_JSON),
            _ => {
                // XX(@carodewig) this should be unreachable, but provide fallback of APPLICATION_JSON
                HeaderValue::from_static(APPLICATION_JSON)
            }
        };
        headers.insert(CONTENT_TYPE, content_type);

        response
    }
}

#[async_trait::async_trait]
impl Plugin for ContentNegotiation {
    type Config = Config;

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Ok(ContentNegotiation {})
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .checkpoint(|request: router::Request| Ok(Self::handle_request(request)))
            .service(service)
            .map_response(Self::handle_response)
            .boxed()
    }
}

/// Returns `true` if the media type is either `application/json` or `application/graphql-response+json`.
fn is_json_type(mime: &MediaType) -> bool {
    use mediatype::names::APPLICATION;
    use mediatype::names::JSON;
    let is_json = |mime: &MediaType| mime.subty == JSON;
    let is_gql_json =
        |mime: &MediaType| mime.subty.as_str() == "graphql-response" && mime.suffix == Some(JSON);

    mime.ty == APPLICATION && (is_json(mime) || is_gql_json(mime))
}

/// Returns `true` if the media type is `*/*`.
fn is_wildcard(mime: &MediaType) -> bool {
    use mediatype::names::_STAR;
    mime.ty == _STAR && mime.subty == _STAR
}

/// Returns `true` if media type is a multipart defer, ie `multipart/mixed;deferSpec=20220824`.
fn is_multipart_defer(mime: &MediaType) -> bool {
    use mediatype::names::MIXED;
    use mediatype::names::MULTIPART;

    let Some(parameter) = mediatype::Name::new(MULTIPART_DEFER_SPEC_PARAMETER) else {
        return false;
    };
    let Some(value) = mediatype::Value::new(MULTIPART_DEFER_SPEC_VALUE) else {
        return false;
    };

    mime.ty == MULTIPART && mime.subty == MIXED && mime.get_param(parameter) == Some(value)
}

/// Returns `true` if media type is a multipart subscription, ie `multipart/mixed;subscriptionSpec=1.0`.
fn is_multipart_subscription(mime: &MediaType) -> bool {
    use mediatype::names::MIXED;
    use mediatype::names::MULTIPART;

    let Some(parameter) = mediatype::Name::new(MULTIPART_SUBSCRIPTION_SPEC_PARAMETER) else {
        return false;
    };
    let Some(value) = mediatype::Value::new(MULTIPART_SUBSCRIPTION_SPEC_VALUE) else {
        return false;
    };

    mime.ty == MULTIPART && mime.subty == MIXED && mime.get_param(parameter) == Some(value)
}

/// Returns `true` if the `CONTENT_TYPE` header contains `application/json` or
/// `application/graphql-response+json`.
fn content_type_includes_json(headers: &HeaderMap) -> bool {
    headers
        .get_all(CONTENT_TYPE)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(MediaTypeList::new)
        .any(|mime_result| mime_result.as_ref().is_ok_and(is_json_type))
}

/// Builds and returns `ClientRequestAccepts` from the `ACCEPT` content header.
fn parse_accept_header(headers: &HeaderMap) -> ClientRequestAccepts {
    let mut accept_header_present = false;
    let mut accepts = ClientRequestAccepts::default();

    headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|header| {
            accept_header_present = true;
            header.to_str().ok()
        })
        .flat_map(MediaTypeList::new)
        .flatten()
        .for_each(|mime| {
            accepts.json = accepts.json || is_json_type(&mime);
            accepts.wildcard = accepts.wildcard || is_wildcard(&mime);
            accepts.multipart_defer = accepts.multipart_defer || is_multipart_defer(&mime);
            accepts.multipart_subscription =
                accepts.multipart_subscription || is_multipart_subscription(&mime);
        });

    if !accept_header_present {
        accepts.json = true;
    }

    accepts
}

/// Process the headers to make sure that `VARY` is set correctly.
fn process_vary_header(headers: &mut HeaderMap<HeaderValue>) {
    if headers.get(VARY).is_none() {
        // We don't have a VARY header, add one with value "origin"
        headers.insert(VARY, ORIGIN_HEADER_VALUE);
    }
}

/// Helper to build a `RouterBody` containing a `graphql::Error` with the provided extension
/// code and message.
fn error_response_body(extension_code: &str, message: String) -> RouterBody {
    router::body::from_bytes(
        serde_json::json!({
            "errors": [
                graphql::Error::builder()
                    .message(message)
                    .extension_code(extension_code)
                    .build()
            ]
        })
        .to_string(),
    )
}

/// Helper to build an HTTP response with a standardized invalid `CONTENT_TYPE` header message.
fn invalid_content_type_header_response() -> http::Response<RouterBody> {
    let message = format!(
        r#"'content-type' header must be one of: {:?} or {:?}"#,
        APPLICATION_JSON, APPLICATION_GRAPHQL_JSON,
    );
    http::Response::builder()
        .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        .header(CONTENT_TYPE, HeaderValue::from_static(APPLICATION_JSON))
        .body(error_response_body("INVALID_CONTENT_TYPE_HEADER", message))
        .expect("cannot fail")
}

/// Helper to build an HTTP response with a standardized invalid `ACCEPT` header message.
pub(crate) fn invalid_accept_header_response() -> http::Response<RouterBody> {
    let message = format!(
        r#"'accept' header must be one of: \"*/*\", {:?}, {:?}, {:?} or {:?}"#,
        APPLICATION_JSON,
        APPLICATION_GRAPHQL_JSON,
        MULTIPART_SUBSCRIPTION_ACCEPT,
        MULTIPART_DEFER_ACCEPT_HEADER_VALUE
    );
    http::Response::builder()
        .status(StatusCode::NOT_ACCEPTABLE)
        .header(CONTENT_TYPE, HeaderValue::from_static(APPLICATION_JSON))
        .body(error_response_body("INVALID_ACCEPT_HEADER", message))
        .expect("cannot fail")
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::header::HeaderValue;
    use http::header::VARY;

    use super::APPLICATION_GRAPHQL_JSON;
    use super::APPLICATION_JSON;
    use super::MULTIPART_DEFER_ACCEPT_HEADER_VALUE;
    use super::content_type_includes_json;
    use super::parse_accept_header;
    use super::process_vary_header;

    const VALID_CONTENT_TYPES: [&str; 2] = [APPLICATION_JSON, APPLICATION_GRAPHQL_JSON];
    const INVALID_CONTENT_TYPES: [&str; 3] = ["invalid", "application/invalid", "application/yaml"];

    #[test]
    fn test_content_type_includes_json_handles_valid_content_types() {
        for content_type in VALID_CONTENT_TYPES {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            assert!(content_type_includes_json(&headers));
        }
    }

    #[test]
    fn test_content_type_includes_json_handles_invalid_content_types() {
        for content_type in INVALID_CONTENT_TYPES {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            assert!(!content_type_includes_json(&headers));
        }
    }

    #[test]
    fn test_content_type_includes_json_can_process_multiple_content_types() {
        let mut headers = HeaderMap::new();
        for content_type in INVALID_CONTENT_TYPES {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        }
        for content_type in VALID_CONTENT_TYPES {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        }

        assert!(content_type_includes_json(&headers));

        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            INVALID_CONTENT_TYPES.join(", ").parse().unwrap(),
        );
        headers.insert(
            CONTENT_TYPE,
            VALID_CONTENT_TYPES.join(", ").parse().unwrap(),
        );
        assert!(content_type_includes_json(&headers));
    }

    #[test]
    fn test_parse_accept_header_behaves_as_expected() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static(VALID_CONTENT_TYPES[0]));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.json);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.wildcard);

        let mut default_headers = HeaderMap::new();
        // real life browser example
        default_headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"));
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.wildcard);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static(APPLICATION_GRAPHQL_JSON));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.json);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static(APPLICATION_GRAPHQL_JSON));
        default_headers.append(ACCEPT, MULTIPART_DEFER_ACCEPT_HEADER_VALUE);
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.multipart_defer);

        // Multiple accepted types, including one with a parameter we are interested in
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static("multipart/mixed;subscriptionSpec=1.0, application/json"),
        );
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.multipart_subscription);

        // No accept header present
        let default_headers = HeaderMap::new();
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.json);
    }

    #[test]
    fn it_adds_default_with_value_origin_if_no_vary_header() {
        let mut default_headers = HeaderMap::new();
        process_vary_header(&mut default_headers);
        let vary_opt = default_headers.get(VARY);
        assert!(vary_opt.is_some());
        let vary = vary_opt.expect("has a value");
        assert_eq!(vary, "origin");
    }

    #[test]
    fn it_leaves_vary_alone_if_set() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(VARY, HeaderValue::from_static("*"));
        process_vary_header(&mut default_headers);
        let vary_opt = default_headers.get(VARY);
        assert!(vary_opt.is_some());
        let vary = vary_opt.expect("has a value");
        assert_eq!(vary, "*");
    }

    #[test]
    fn it_leaves_varys_alone_if_there_are_more_than_one() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(VARY, HeaderValue::from_static("one"));
        default_headers.append(VARY, HeaderValue::from_static("two"));
        process_vary_header(&mut default_headers);
        let vary = default_headers.get_all(VARY);
        assert_eq!(vary.iter().count(), 2);
        for value in vary {
            assert!(value == "one" || value == "two");
        }
    }
}
