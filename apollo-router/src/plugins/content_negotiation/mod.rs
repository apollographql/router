//! Layers that do HTTP content negotiation using the Accept and Content-Type headers.
//!
//! Content negotiation uses a pair of layers that work together at the router and supergraph stages.
use std::ops::ControlFlow;

use http::HeaderMap;
use http::Method;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use mime::APPLICATION_JSON;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::MULTIPART_DEFER_ACCEPT;
use crate::services::MULTIPART_DEFER_SPEC_PARAMETER;
use crate::services::MULTIPART_DEFER_SPEC_VALUE;
use crate::services::MULTIPART_SUBSCRIPTION_ACCEPT;
use crate::services::MULTIPART_SUBSCRIPTION_SPEC_PARAMETER;
use crate::services::MULTIPART_SUBSCRIPTION_SPEC_VALUE;
use crate::services::router;
use crate::services::router::ClientRequestAccepts;
use crate::services::router::body::RouterBody;
use crate::services::router::service::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::router::service::MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE;
use crate::services::router::service::MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE;
use crate::services::supergraph;

/// TODO: unify the following doc comments
/// from RouterLayer
/// A layer for the router service that rejects requests that do not have an expected Content-Type,
/// or that have an Accept header that is not supported by the router.
///
/// In particular, the Content-Type must be JSON, and the Accept header must include */*, or one of
/// the JSON/GraphQL MIME types.
///
/// # Context
/// If the request is valid, this layer adds a [`ClientRequestAccepts`] value to the context.
///
///
/// from SupergraphLayer
/// A layer for the supergraph service that populates the Content-Type response header.
///
/// The content type is decided based on the [`ClientRequestAccepts`] context value, which is
/// populated by the content negotiation [`RouterLayer`].
// XXX(@goto-bus-stop): this feels a bit odd. It probably works fine because we can only ever respond
// with JSON, but maybe this should be done as close as possible to where we populate the response body..?
struct ContentNegotiation {}
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Config {}

impl ContentNegotiation {
    // TODO: see if there's already an implementation of this
    fn response_body(extension_code: &str, message: String) -> RouterBody {
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

    fn invalid_content_type_header_response() -> http::Response<RouterBody> {
        let message = format!(
            r#"'content-type' header must be one of: {:?} or {:?}"#,
            APPLICATION_JSON.essence_str(),
            GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
        );
        http::Response::builder()
            .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .body(Self::response_body("INVALID_CONTENT_TYPE_HEADER", message))
            .expect("cannot fail")
    }

    fn invalid_accept_header_response() -> http::Response<RouterBody> {
        let message = format!(
            r#"'accept' header must be one of: \"*/*\", {:?}, {:?}, {:?} or {:?}"#,
            APPLICATION_JSON.essence_str(),
            GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
            MULTIPART_SUBSCRIPTION_ACCEPT,
            MULTIPART_DEFER_ACCEPT
        );
        http::Response::builder()
            .status(StatusCode::NOT_ACCEPTABLE)
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .body(Self::response_body("INVALID_ACCEPT_HEADER", message))
            .expect("cannot fail")
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

    /// A layer for the router service that rejects requests that do not have an expected Content-Type,
    /// or that have an Accept header that is not supported by the router.
    ///
    /// In particular, the Content-Type must be JSON, and the Accept header must include */*, or one of
    /// the JSON/GraphQL MIME types.
    ///
    /// # Context
    /// If the request is valid, this layer adds a [`ClientRequestAccepts`] value to the context.
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .checkpoint(|request: router::Request| {
                let valid_content_type_header = request.router_request.method() == Method::GET
                    || content_type_includes_json(request.router_request.headers());

                if valid_content_type_header {
                    Ok(ControlFlow::Continue(request))
                } else {
                    Ok(ControlFlow::Break(
                        Self::invalid_content_type_header_response().into(),
                    ))
                }
            })
            .checkpoint(|request: router::Request| {
                let accepts = parse_accept_header(request.router_request.headers());
                let valid_accept_header = accepts.json
                    || accepts.wildcard
                    || accepts.multipart_defer
                    || accepts.multipart_subscription;

                if valid_accept_header {
                    request
                        .context
                        .extensions()
                        .with_lock(|lock| lock.insert(accepts));
                    Ok(ControlFlow::Continue(request))
                } else {
                    Ok(ControlFlow::Break(
                        Self::invalid_accept_header_response().into(),
                    ))
                }
            })
            .service(service)
            .boxed()
    }

    /// A layer for the supergraph service that populates the Content-Type response header.
    ///
    /// The content type is decided based on the [`ClientRequestAccepts`] context value, which is
    /// populated by the content negotiation [`RouterLayer`].
    //
    // XXX(@goto-bus-stop): this feels a bit odd. It probably works fine because we can only ever respond
    // with JSON, but maybe this should be done as close as possible to where we populate the response body..?
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
            .map_response(|mut response: supergraph::Response| {
                let ClientRequestAccepts {
                    wildcard: accepts_wildcard,
                    json: accepts_json,
                    multipart_defer: accepts_multipart_defer,
                    multipart_subscription: accepts_multipart_subscription,
                } = response.context.extensions().with_lock(|lock| {
                    lock.get::<ClientRequestAccepts>()
                        .cloned()
                        .unwrap_or_default()
                });

                let headers = response.response.headers_mut();
                if accepts_json || accepts_wildcard {
                    headers.insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
                } else if accepts_multipart_defer {
                    headers.insert(
                        CONTENT_TYPE,
                        MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE.clone(),
                    );
                } else if accepts_multipart_subscription {
                    headers.insert(
                        CONTENT_TYPE,
                        MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE.clone(),
                    );
                }
                response
            })
            .service(service)
            .boxed()
    }
}

fn is_json_type(mime: &MediaType) -> bool {
    use mediatype::names::APPLICATION;
    use mediatype::names::JSON;
    let is_json = |mime: &MediaType| mime.subty == JSON;
    let is_gql_json =
        |mime: &MediaType| mime.subty.as_str() == "graphql-response" && mime.suffix == Some(JSON);

    mime.ty == APPLICATION && (is_json(mime) || is_gql_json(mime))
}

fn is_wildcard(mime: &MediaType) -> bool {
    use mediatype::names::_STAR;
    mime.ty == _STAR && mime.subty == _STAR
}

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

/// Returns true if the `CONTENT_TYPE` header contains `application/json` or
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

register_plugin!("apollo", "content_negotiation", ContentNegotiation);

#[cfg(test)]
mod tests {
    use http::HeaderMap;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::header::HeaderValue;

    use super::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
    use super::content_type_includes_json;
    use super::parse_accept_header;
    use crate::services::MULTIPART_DEFER_ACCEPT;

    const VALID_CONTENT_TYPES: [&str; 2] = ["application/json", GRAPHQL_JSON_RESPONSE_HEADER_VALUE];
    const INVALID_CONTENT_TYPES: [&str; 3] = ["invalid", "application/invalid", "application/yaml"];

    #[test]
    fn test_content_type_includes_json_handles_valid_content_types() {
        for content_type in VALID_CONTENT_TYPES {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, content_type.parse().unwrap());
            assert!(content_type_includes_json(&headers));
        }
    }

    #[test]
    fn test_content_type_includes_json_handles_invalid_content_types() {
        for content_type in INVALID_CONTENT_TYPES {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, content_type.parse().unwrap());
            assert!(!content_type_includes_json(&headers));
        }
    }

    #[test]
    fn test_content_type_includes_json_can_process_multiple_content_types() {
        let mut headers = HeaderMap::new();
        for content_type in INVALID_CONTENT_TYPES {
            headers.insert(CONTENT_TYPE, content_type.parse().unwrap());
        }
        for content_type in VALID_CONTENT_TYPES {
            headers.insert(CONTENT_TYPE, content_type.parse().unwrap());
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
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept_header(&default_headers);
        assert!(accepts.json);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static(MULTIPART_DEFER_ACCEPT));
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
}
