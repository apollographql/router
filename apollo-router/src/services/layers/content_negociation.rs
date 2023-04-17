use std::ops::ControlFlow;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::HeaderMap;
use http::HeaderValue;
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
use crate::services::supergraph;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::services::MULTIPART_DEFER_SPEC_PARAMETER;
use crate::services::MULTIPART_DEFER_SPEC_VALUE;

pub(crate) const GRAPHQL_JSON_RESPONSE_HEADER_VALUE: &str = "application/graphql-response+json";
pub(crate) const ACCEPTS_WILDCARD_CONTEXT_KEY: &str = "content-negociation:accepts-wildcard";
pub(crate) const ACCEPTS_MULTIPART_CONTEXT_KEY: &str = "content-negociation:accepts-multipart";
pub(crate) const ACCEPTS_JSON_CONTEXT_KEY: &str = "content-negociation:accepts-json";

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
                let accepts_multipart = accepts_multipart(req.router_request.headers());
                let accepts_json = accepts_json(req.router_request.headers());
                let accepts_wildcard = accepts_wildcard(req.router_request.headers());

                if accepts_wildcard || accepts_multipart || accepts_json {
                    req.context
                        .insert(ACCEPTS_WILDCARD_CONTEXT_KEY, accepts_wildcard)
                        .unwrap();
                    req.context
                        .insert(ACCEPTS_MULTIPART_CONTEXT_KEY, accepts_multipart)
                        .unwrap();
                    req.context
                        .insert(ACCEPTS_JSON_CONTEXT_KEY, accepts_json)
                        .unwrap();

                    Ok(ControlFlow::Continue(req))
                } else {
                    let response: http::Response<hyper::Body> = http::Response::builder().status(StatusCode::NOT_ACCEPTABLE).header(CONTENT_TYPE, APPLICATION_JSON.essence_str()).body(
                            hyper::Body::from(
                                serde_json::json!({
                                    "errors": [
                                        graphql::Error::builder()
                                            .message(format!(
                                                r#"'accept' header must be one of: \"*/*\", {:?}, {:?} or {:?}"#,
                                                APPLICATION_JSON.essence_str(),
                                                GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
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
                let accepts_wildcard: bool = context
                    .get(ACCEPTS_WILDCARD_CONTEXT_KEY)
                    .unwrap_or_default()
                    .unwrap_or_default();
                let accepts_json: bool = context
                    .get(ACCEPTS_JSON_CONTEXT_KEY)
                    .unwrap_or_default()
                    .unwrap_or_default();
                let accepts_multipart: bool = context
                    .get(ACCEPTS_MULTIPART_CONTEXT_KEY)
                    .unwrap_or_default()
                    .unwrap_or_default();

                if !res.has_next.unwrap_or_default() && (accepts_json || accepts_wildcard) {
                    parts.headers.insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static(APPLICATION_JSON.essence_str()),
                    );
                } else if accepts_multipart {
                    parts.headers.insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
                    );
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

/// Returns true if the headers contain `accept: application/json` or `accept: application/graphql-response+json`,
/// or if there is no `accept` header
fn accepts_json(headers: &HeaderMap) -> bool {
    !headers.contains_key(ACCEPT)
        || headers.get_all(ACCEPT).iter().any(|value| {
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

/// Returns true if the headers contain header `accept: */*`
fn accepts_wildcard(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| {
                    mime.as_ref()
                        .map(|mime| (mime.ty == _STAR && mime.subty == _STAR))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

/// Returns true if the headers contain accept header to enable defer
fn accepts_multipart(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| {
                    mime.as_ref()
                        .map(|mime| {
                            mime.ty == MULTIPART
                                && mime.subty == MIXED
                                && mime.get_param(
                                    mediatype::Name::new(MULTIPART_DEFER_SPEC_PARAMETER)
                                        .expect("valid name"),
                                ) == Some(
                                    mediatype::Value::new(MULTIPART_DEFER_SPEC_VALUE)
                                        .expect("valid value"),
                                )
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_checks_accept_header() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        assert!(accepts_json(&default_headers));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        assert!(accepts_wildcard(&default_headers));

        let mut default_headers = HeaderMap::new();
        // real life browser example
        default_headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"));
        assert!(accepts_wildcard(&default_headers));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        assert!(accepts_json(&default_headers));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(
            ACCEPT,
            HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
        );
        assert!(accepts_multipart(&default_headers));
    }
}
