//! Layers that do HTTP content negotiation using the Accept and Content-Type headers.
//!
//! Content negotiation uses pairs of layers that work together at the router, supergraph, and
//! subgraph stages.

use std::ops::ControlFlow;
use std::task::Poll;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Either;
use http::HeaderMap;
use http::Method;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::response::Parts;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use mediatype::names::_STAR;
use mediatype::names::APPLICATION;
use mediatype::names::JSON;
use mediatype::names::MIXED;
use mediatype::names::MULTIPART;
use mime::APPLICATION_JSON;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::FetchError;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::layers::ServiceExt as _;
use crate::layers::async_checkpoint::AsyncCheckpointService;
use crate::services::MULTIPART_DEFER_ACCEPT;
use crate::services::MULTIPART_DEFER_SPEC_PARAMETER;
use crate::services::MULTIPART_DEFER_SPEC_VALUE;
use crate::services::MULTIPART_SUBSCRIPTION_ACCEPT;
use crate::services::MULTIPART_SUBSCRIPTION_SPEC_PARAMETER;
use crate::services::MULTIPART_SUBSCRIPTION_SPEC_VALUE;
use crate::services::execution;
use crate::services::router;
use crate::services::router::ClientRequestAccepts;
use crate::services::router::service::MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE;
use crate::services::router::service::MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE;
use crate::services::subgraph;
use crate::services::subgraph::http::APPLICATION_JSON_HEADER_VALUE;
use crate::services::supergraph;

pub(crate) const GRAPHQL_JSON_RESPONSE_HEADER_VALUE: &str = "application/graphql-response+json";

const GRAPHQL_RESPONSE: mediatype::Name = mediatype::Name::new_unchecked("graphql-response");

#[allow(clippy::declare_interior_mutable_const)]
pub(crate) static ACCEPT_GRAPHQL_JSON: http::HeaderValue =
    http::HeaderValue::from_static("application/json, application/graphql-response+json");

#[derive(Clone, Debug)]
pub(crate) enum ContentType {
    ApplicationJson,
    ApplicationGraphqlResponseJson,
}

pub(crate) fn get_graphql_content_type(
    service_name: &str,
    parts: &Parts,
) -> Result<ContentType, FetchError> {
    if let Some(raw_content_type) = parts.headers.get(CONTENT_TYPE) {
        let content_type = raw_content_type
            .to_str()
            .ok()
            .and_then(|str| MediaType::parse(str).ok());

        match content_type {
            Some(mime) if mime.ty == APPLICATION && mime.subty == JSON => {
                Ok(ContentType::ApplicationJson)
            }
            Some(mime)
                if mime.ty == APPLICATION
                    && mime.subty == GRAPHQL_RESPONSE
                    && mime.suffix == Some(JSON) =>
            {
                Ok(ContentType::ApplicationGraphqlResponseJson)
            }
            Some(mime) => Err(format!(
                "subgraph response contains unsupported content-type: {mime}",
            )),
            None => Err(format!(
                "subgraph response contains invalid 'content-type' header value {raw_content_type:?}",
            )),
        }
    } else {
        Err("subgraph response does not contain 'content-type' header".to_owned())
    }
    .map_err(|reason| FetchError::SubrequestHttpError {
        status_code: Some(parts.status.as_u16()),
        service: service_name.to_string(),
        reason: format!(
            "{}; expected content-type: {} or content-type: {}",
            reason,
            APPLICATION_JSON.essence_str(),
            GRAPHQL_JSON_RESPONSE_HEADER_VALUE
        ),
    })
}

/// Sets the outbound `Content-Type` to `application/json` and appends an `Accept` header
/// advertising support for both GraphQL-over-HTTP response media types.
///
/// Used by [`SubgraphContentNegotiationLayer`]. Batched subgraph requests get these headers too, since each request
/// making up a batch passes through `SubgraphContentNegotiationLayer` before being diverted into batching.
pub(crate) fn inject_subgraph_request_headers(headers: &mut HeaderMap) {
    headers.insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
    headers.append(ACCEPT, ACCEPT_GRAPHQL_JSON.clone());
}

/// A layer for the subgraph service that injects `Accept` and `Content-Type` headers on outbound
/// requests. Content-type validation and HTTP-to-GraphQL response conversion still happen inline
/// in the subgraph service, since they operate on the response side rather than the request.
#[derive(Clone, Default)]
pub(crate) struct SubgraphContentNegotiationLayer {}

impl<S> Layer<S> for SubgraphContentNegotiationLayer
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as Service<subgraph::Request>>::Future: Send + 'static,
{
    type Service = SubgraphContentNegotiationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SubgraphContentNegotiationService { inner }
    }
}

pub(crate) struct SubgraphContentNegotiationService<S> {
    inner: S,
}

impl<S: Clone> Clone for SubgraphContentNegotiationService<S> {
    fn clone(&self) -> Self {
        SubgraphContentNegotiationService {
            inner: self.inner.clone(),
        }
    }
}

impl<S> Service<subgraph::Request> for SubgraphContentNegotiationService<S>
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = subgraph::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: subgraph::Request) -> Self::Future {
        inject_subgraph_request_headers(request.subgraph_request.headers_mut());
        self.inner.call(request)
    }
}

/// A layer for the router service that rejects requests that do not have an expected Content-Type,
/// or that have an Accept header that is not supported by the router.
///
/// In particular, the Content-Type must be JSON, and the Accept header must include */*, or one of
/// the JSON/GraphQL MIME types.
///
/// # Context
/// If the request is valid, this layer adds a [`ClientRequestAccepts`] value to the context.
#[derive(Clone, Default)]
pub(crate) struct RouterContentNegotiationLayer {}

impl<S> Layer<S> for RouterContentNegotiationLayer
where
    S: Service<router::Request, Response = router::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    <S as Service<router::Request>>::Future: Send + 'static,
{
    type Service = AsyncCheckpointService<
        S,
        BoxFuture<'static, Result<ControlFlow<router::Response, router::Request>, BoxError>>,
        router::Request,
    >;

    fn layer(&self, service: S) -> Self::Service {
        ServiceBuilder::new()
            .checkpoint_async(move |req: router::Request| {
                async move {
                    if req.router_request.method() != Method::GET
                        && !content_type_is_json(req.router_request.headers())
                    {
                        let response = http::Response::builder()
                        .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(router::body::from_bytes(
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

                    if req.router_request.method() == Method::GET
                        && !content_type_is_strictly_json_or_missing(req.router_request.headers())
                    {
                        let response = http::Response::builder()
                        .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(router::body::from_bytes(
                            serde_json::json!({
                                "errors": [
                                    graphql::Error::builder()
                                        .message(format!("GET request 'content-type' header may only contain: {:?}", APPLICATION_JSON.essence_str()))
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
                        req.context
                            .extensions()
                            .with_lock(|lock| lock.insert(accepts));

                        Ok(ControlFlow::Continue(req))
                    } else {
                        let response = http::Response::builder()
                        .status(StatusCode::NOT_ACCEPTABLE)
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(router::body::from_bytes(
                            serde_json::json!({
                                "errors": [
                                    graphql::Error::builder()
                                        .message(format!(
                                            r#"'accept' header must be one of: \"*/*\", {:?}, {:?}, {:?} or {:?}"#,
                                            APPLICATION_JSON.essence_str(),
                                            GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                            MULTIPART_SUBSCRIPTION_ACCEPT,
                                            MULTIPART_DEFER_ACCEPT
                                        ))
                                        .extension_code("INVALID_ACCEPT_HEADER")
                                        .build()
                                ]
                            })
                            .to_string()
                        )).expect("cannot fail");

                        Ok(ControlFlow::Break(response.into()))
                    }
                }
                .boxed()
            })
            .service(service)
    }
}

/// A layer for the supergraph service that populates the Content-Type response header.
///
/// The content type is decided based on the [`ClientRequestAccepts`] context value, which is
/// populated by the content negotiation [`RouterContentNegotiationLayer`].
// XXX(@goto-bus-stop): this feels a bit odd. It probably works fine because we can only ever respond
// with JSON, but maybe this should be done as close as possible to where we populate the response body..?
#[derive(Clone, Default)]
pub(crate) struct SupergraphContentNegotiationLayer {}

impl<S> Layer<S> for SupergraphContentNegotiationLayer
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    <S as Service<supergraph::Request>>::Future: Send + 'static,
{
    type Service = supergraph::BoxCloneService;

    fn layer(&self, service: S) -> Self::Service {
        service
            .map_first_graphql_response(|context, mut parts, res| {
                let ClientRequestAccepts {
                    wildcard: accepts_wildcard,
                    json: accepts_json,
                    multipart_defer: accepts_multipart_defer,
                    multipart_subscription: accepts_multipart_subscription,
                } = context.extensions().with_lock(|lock| {
                    lock.get::<ClientRequestAccepts>()
                        .cloned()
                        .unwrap_or_default()
                });

                if !res.has_next.unwrap_or_default() && (accepts_json || accepts_wildcard) {
                    parts
                        .headers
                        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
                } else if accepts_multipart_defer {
                    parts.headers.insert(
                        CONTENT_TYPE,
                        MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE.clone(),
                    );
                } else if accepts_multipart_subscription {
                    parts.headers.insert(
                        CONTENT_TYPE,
                        MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE.clone(),
                    );
                }
                (parts, res)
            })
            .boxed_clone()
    }
}

/// Returns true if no content type was provided, or if content type's MIME type is `application/json`
/// (including any optional parameters, ie `; charset=utf-8`).
/// Returns false if any other types are provided or if any of the types are malformed.
// NB: content type can come in through (1) multiple header values and (2) multiple elements within the
//     same header value, so checking this is kind of a pain
fn content_type_is_strictly_json_or_missing(headers: &HeaderMap) -> bool {
    for header_value in headers.get_all(CONTENT_TYPE) {
        let Ok(content_type_str) = header_value.to_str() else {
            return false;
        };

        let mime_results = MediaTypeList::new(content_type_str);
        for mime_result in mime_results {
            let Ok(mime) = mime_result else { return false };
            if !(mime.ty == APPLICATION && mime.subty == JSON) {
                return false;
            }
        }
    }

    true
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

/// A layer for the execution service that checks that the query plan's contents are compatible with
/// the request's `Accept` header.
#[derive(Clone, Default)]
pub(crate) struct QueryPlanCheckAcceptLayer {
    _private: (),
}

impl QueryPlanCheckAcceptLayer {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for QueryPlanCheckAcceptLayer {
    type Service = QueryPlanCheckAcceptService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        QueryPlanCheckAcceptService::new(inner)
    }
}

#[derive(Clone)]
pub(crate) struct QueryPlanCheckAcceptService<S> {
    inner: S,
}

impl<S> QueryPlanCheckAcceptService<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> tower::Service<execution::Request> for QueryPlanCheckAcceptService<S>
where
    S: tower::Service<execution::Request, Response = execution::Response>,
    S::Future: Send + 'static,
{
    type Response = execution::Response;
    type Error = S::Error;
    type Future = Either<S::Future, std::future::Ready<Result<Self::Response, Self::Error>>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: execution::Request) -> Self::Future {
        let context = &req.context;
        let variables = &req.supergraph_request.body().variables;

        let is_deferred = req.query_plan.is_deferred(variables);
        let is_subscription = req.query_plan.is_subscription();

        let ClientRequestAccepts {
            multipart_defer: accepts_multipart_defer,
            multipart_subscription: accepts_multipart_subscription,
            ..
        } = context
            .extensions()
            .with_lock(|lock| lock.get().cloned())
            .unwrap_or_default();
        if (is_deferred && !accepts_multipart_defer)
            || (is_subscription && !accepts_multipart_subscription)
        {
            let (error_message, error_code) = if is_deferred {
                (
                    "the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed;deferSpec=20220824'",
                    "DEFER_BAD_HEADER",
                )
            } else {
                (
                    "the router received a query with a subscription but the client does not accept multipart/mixed HTTP responses. To enable subscription support, add the HTTP header 'Accept: multipart/mixed;subscriptionSpec=1.0'",
                    "SUBSCRIPTION_BAD_HEADER",
                )
            };
            let response = execution::Response::error_builder()
                .status_code(StatusCode::NOT_ACCEPTABLE)
                .error(
                    graphql::Error::builder()
                        .message(error_message)
                        .extension_code(error_code)
                        .build(),
                )
                .context(req.context)
                .build()
                .expect("does not fail");
            Either::Right(std::future::ready(Ok(response)))
        } else {
            Either::Left(self.inner.call(req))
        }
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use http::StatusCode;
    use tower::ServiceExt as _;

    use super::*;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    #[rstest::rstest]
    #[case::empty(HeaderMap::new())]
    #[case::no_content_type(HeaderMap::from_iter([(ACCEPT, HeaderValue::from_static("*/*"))]))]
    #[case::empty_str(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static(""))]))]
    #[case::application_json(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("application/json"))]))]
    #[case::application_json_with_charset(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"))]))]
    fn content_type_is_strictly_json_or_missing_accepts_valid_headers(#[case] headers: HeaderMap) {
        assert!(content_type_is_strictly_json_or_missing(&headers));
    }

    #[rstest::rstest]
    #[case::text_plan(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("invalid"))]))]
    #[case::text_plan(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("text/plain"))]))]
    #[case::multipart(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("multipart/form-data"))]))]
    #[case::multipart(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("application/graphql"))]))]
    #[case::multiple_values(HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("application/json")), (CONTENT_TYPE, HeaderValue::from_static("text/plain"))]))]
    fn content_type_is_strictly_json_or_missing_rejects_invalid_headers(
        #[case] headers: HeaderMap,
    ) {
        assert!(!content_type_is_strictly_json_or_missing(&headers));
    }

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
        default_headers.append(ACCEPT, HeaderValue::from_static(MULTIPART_DEFER_ACCEPT));
        let accepts = parse_accept(&default_headers);
        assert!(accepts.multipart_defer);

        // Multiple accepted types, including one with a parameter we are interested in
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static("multipart/mixed;subscriptionSpec=1.0, application/json"),
        );
        let accepts = parse_accept(&default_headers);
        assert!(accepts.multipart_subscription);
    }

    #[tokio::test]
    async fn subgraph_layer_injects_accept_and_content_type_headers() {
        use std::sync::Arc;
        use std::sync::Mutex;

        let captured: Arc<Mutex<Option<http::HeaderMap>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let inner = tower::service_fn(move |req: SubgraphRequest| {
            *captured_clone.lock().unwrap() = Some(req.subgraph_request.headers().clone());
            async move { Ok::<_, tower::BoxError>(SubgraphResponse::fake_builder().build()) }
        });

        let mut svc = SubgraphContentNegotiationLayer::default().layer(inner);
        let req = SubgraphRequest::fake_builder().build();
        svc.ready().await.unwrap().call(req).await.unwrap();

        let headers = captured.lock().unwrap().take().unwrap();
        assert_eq!(
            headers.get(ACCEPT).unwrap(),
            "application/json, application/graphql-response+json"
        );
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
    }

    #[test]
    fn get_graphql_content_type_accepts_application_json() {
        let (parts, _) = http::Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            get_graphql_content_type("svc", &parts),
            Ok(ContentType::ApplicationJson)
        ));
    }

    #[test]
    fn get_graphql_content_type_accepts_graphql_response_json() {
        let (parts, _) = http::Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/graphql-response+json")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            get_graphql_content_type("svc", &parts),
            Ok(ContentType::ApplicationGraphqlResponseJson)
        ));
    }

    #[test]
    fn get_graphql_content_type_rejects_missing_header() {
        let (parts, _) = http::Response::builder()
            .status(StatusCode::OK)
            .body(())
            .unwrap()
            .into_parts();
        assert!(get_graphql_content_type("svc", &parts).is_err());
    }

    #[test]
    fn get_graphql_content_type_rejects_unsupported_type() {
        let (parts, _) = http::Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/plain")
            .body(())
            .unwrap()
            .into_parts();
        assert!(get_graphql_content_type("svc", &parts).is_err());
    }
}
