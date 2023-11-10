use std::ops::ControlFlow;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::HeaderMap;
use http::Method;
use http::StatusCode;
use mediatype::names::APPLICATION;
use mediatype::names::JSON;
use mediatype::names::MIXED;
use mediatype::names::MULTIPART;
use mediatype::names::_STAR;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use mime::APPLICATION_JSON;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::sync_checkpoint::CheckpointService;
use crate::layers::ServiceExt as _;
use crate::services::router;
use crate::services::router::ClientRequestAccepts;
use crate::services::router_service::MULTIPART_DEFER_HEADER_VALUE;
use crate::services::router_service::MULTIPART_SUBSCRIPTION_HEADER_VALUE;
use crate::services::supergraph;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::services::MULTIPART_DEFER_SPEC_PARAMETER;
use crate::services::MULTIPART_DEFER_SPEC_VALUE;
use crate::services::MULTIPART_SUBSCRIPTION_CONTENT_TYPE;
use crate::services::MULTIPART_SUBSCRIPTION_SPEC_PARAMETER;
use crate::services::MULTIPART_SUBSCRIPTION_SPEC_VALUE;

pub(crate) const GRAPHQL_JSON_RESPONSE_HEADER_VALUE: &str = "application/graphql-response+json";
/// [`Layer`] for Content-Type checks implementation.
#[derive(Clone, Default)]
pub(crate) struct RouterLayer {}

impl<S> Layer<S> for RouterLayer
where
    S: Service<router::Request, Response = router::Response, Error = BoxError> + Send + 'static,
    <S as Service<router::Request>>::Future: Send + 'static,
{
    type Service = CheckpointService<S, router::Request>;

    fn layer(&self, service: S) -> Self::Service {
        CheckpointService::new(
            move |req| {
                if req.router_request.method() != Method::GET
                    && !content_type_is_json(req.router_request.headers())
                {
                    let response: http::Response<hyper::Body> = http::Response::builder()
                        .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(hyper::Body::from(
                            serde_json::json!({
                                "errors": [
                                    graphql::Error::builder()
                                        .message(format!(
                                            r#"'content-type' header must be one of: {:?} or {:?}"#,
                                            APPLICATION_JSON.essence_str(),
                                            GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                        ))
                                        .extension_code("INVALID_CONTENT_TYPE_HEADER")
                                        .build()
                                ]
                            })
                            .to_string(),
                        ))
                        .expect("cannot fail");

                    return Ok(ControlFlow::Break(response.into()));
                }

                let accepts = parse_accept(req.router_request.headers());

                if accepts.wildcard
                    || accepts.multipart_defer
                    || accepts.multipart_subscription
                    || accepts.json
                {
                    req.context.private_entries.lock().insert(accepts);

                    Ok(ControlFlow::Continue(req))
                } else {
                    let response: http::Response<hyper::Body> = http::Response::builder().status(StatusCode::NOT_ACCEPTABLE).header(CONTENT_TYPE, APPLICATION_JSON.essence_str()).body(
                        hyper::Body::from(
                            serde_json::json!({
                                "errors": [
                                    graphql::Error::builder()
                                        .message(format!(
                                            r#"'accept' header must be one of: \"*/*\", {:?}, {:?}, {:?} or {:?}"#,
                                            APPLICATION_JSON.essence_str(),
                                            GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                            MULTIPART_SUBSCRIPTION_CONTENT_TYPE,
                                            MULTIPART_DEFER_CONTENT_TYPE
                                        ))
                                        .extension_code("INVALID_ACCEPT_HEADER")
                                        .build()
                                ]
                            }).to_string())).expect("cannot fail");

                    Ok(ControlFlow::Break(response.into()))
                }
            },
            service,
        )
    }
}

/// [`Layer`] for Content-Type checks implementation.
#[derive(Clone, Default)]
pub(crate) struct SupergraphLayer {}

impl<S> Layer<S> for SupergraphLayer
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Send
        + 'static,
    <S as Service<supergraph::Request>>::Future: Send + 'static,
{
    type Service = supergraph::BoxService;

    fn layer(&self, service: S) -> Self::Service {
        service
            .map_first_graphql_response(|context, mut parts, res| {
                let ClientRequestAccepts {
                    wildcard: accepts_wildcard,
                    json: accepts_json,
                    multipart_defer: accepts_multipart_defer,
                    multipart_subscription: accepts_multipart_subscription,
                } = context
                    .private_entries
                    .lock()
                    .get()
                    .cloned()
                    .unwrap_or_default();

                if !res.has_next.unwrap_or_default() && (accepts_json || accepts_wildcard) {
                    parts
                        .headers
                        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
                } else if accepts_multipart_defer {
                    parts
                        .headers
                        .insert(CONTENT_TYPE, MULTIPART_DEFER_HEADER_VALUE.clone());
                } else if accepts_multipart_subscription {
                    parts
                        .headers
                        .insert(CONTENT_TYPE, MULTIPART_SUBSCRIPTION_HEADER_VALUE.clone());
                }
                (parts, res)
            })
            .boxed()
    }
}

/// Returns true if the headers content type is `application/json` or `application/graphql-response+json`
fn content_type_is_json(headers: &HeaderMap) -> bool {
    headers.get_all(CONTENT_TYPE).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| {
                    mime.as_ref()
                        .map(|mime| {
                            (mime.ty == APPLICATION && mime.subty == JSON)
                                || (mime.ty == APPLICATION
                                    && mime.subty.as_str() == "graphql-response"
                                    && mime.suffix == Some(JSON))
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}
// Clippy suggests `for mime in MediaTypeList::new(str).flatten()` but less indentation
// does not seem worth making it invisible that Result is involved.
#[allow(clippy::manual_flatten)]
/// Returns (accepts_json, accepts_wildcard, accepts_multipart)
fn parse_accept(headers: &HeaderMap) -> ClientRequestAccepts {
    let mut header_present = false;
    let mut accepts = ClientRequestAccepts::default();
    for value in headers.get_all(ACCEPT) {
        header_present = true;
        if let Ok(str) = value.to_str() {
            for result in MediaTypeList::new(str) {
                if let Ok(mime) = result {
                    if !accepts.json
                        && ((mime.ty == APPLICATION && mime.subty == JSON)
                            || (mime.ty == APPLICATION
                                && mime.subty.as_str() == "graphql-response"
                                && mime.suffix == Some(JSON)))
                    {
                        accepts.json = true
                    }
                    if !accepts.wildcard && (mime.ty == _STAR && mime.subty == _STAR) {
                        accepts.wildcard = true
                    }
                    if !accepts.multipart_defer && (mime.ty == MULTIPART && mime.subty == MIXED) {
                        let parameter = mediatype::Name::new(MULTIPART_DEFER_SPEC_PARAMETER)
                            .expect("valid name");
                        let value =
                            mediatype::Value::new(MULTIPART_DEFER_SPEC_VALUE).expect("valid value");
                        if mime.get_param(parameter) == Some(value) {
                            accepts.multipart_defer = true
                        }
                    }
                    if !accepts.multipart_subscription
                        && (mime.ty == MULTIPART && mime.subty == MIXED)
                    {
                        let parameter = mediatype::Name::new(MULTIPART_SUBSCRIPTION_SPEC_PARAMETER)
                            .expect("valid name");
                        let value = mediatype::Value::new(MULTIPART_SUBSCRIPTION_SPEC_VALUE)
                            .expect("valid value");
                        if mime.get_param(parameter) == Some(value) {
                            accepts.multipart_subscription = true
                        }
                    }
                }
            }
        }
    }
    if !header_present {
        accepts.json = true
    }
    accepts
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    #[test]
    fn it_checks_accept_header() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept(&default_headers);
        assert!(accepts.json);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept(&default_headers);
        assert!(accepts.wildcard);

        let mut default_headers = HeaderMap::new();
        // real life browser example
        default_headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"));
        let accepts = parse_accept(&default_headers);
        assert!(accepts.wildcard);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        let accepts = parse_accept(&default_headers);
        assert!(accepts.json);

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(
            ACCEPT,
            HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
        );
        let accepts = parse_accept(&default_headers);
        assert!(accepts.multipart_defer);
    }
}
