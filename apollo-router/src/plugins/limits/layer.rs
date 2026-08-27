use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::task::Poll;

use displaydoc::Display;
use futures::FutureExt;
use pin_project_lite::pin_project;
use tokio::sync::AcquireError;
use tokio::sync::OwnedSemaphorePermit;
use tower::Layer;
use tower_service::Service;

#[derive(thiserror::Error, Debug, Clone, Copy, Display)]
pub(super) enum RequestSizeLimitError {
    /// Request body payload too large
    BodyTooLarge,
    /// Request query payload too large
    QueryTooLarge,
}

struct BodyLimitControlInner {
    limit: AtomicUsize,
    current: AtomicUsize,
}

/// This structure allows the body limit to be updated dynamically.
/// It also allows the error message to be updated
///
#[derive(Clone)]
pub(crate) struct BodyLimitControl {
    inner: Arc<BodyLimitControlInner>,
}

impl BodyLimitControl {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            inner: Arc::new(BodyLimitControlInner {
                limit: AtomicUsize::new(limit),
                current: AtomicUsize::new(0),
            }),
        }
    }

    /// To disable the limit check just set this to usize::MAX
    #[allow(dead_code)]
    pub(crate) fn update_limit(&self, limit: usize) {
        assert!(
            self.limit() < limit,
            "new limit must be greater than current limit"
        );
        self.inner
            .limit
            .store(limit, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns the current limit, this may have been updated dynamically.
    /// Usually it is the minimum of the content-length header and the configured limit.
    pub(crate) fn limit(&self) -> usize {
        self.inner.limit.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns how much is remaining before the limit is hit
    pub(crate) fn remaining(&self) -> usize {
        self.inner.limit.load(std::sync::atomic::Ordering::SeqCst)
            - self.inner.current.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Increment the current counted bytes by an amount
    pub(crate) fn increment(&self, amount: usize) -> usize {
        self.inner
            .current
            .fetch_add(amount, std::sync::atomic::Ordering::SeqCst)
    }
}

/// This layer differs from the tower version in that it will always generate an error eagerly rather than
/// allowing the downstream service to catch and handle the error.
/// This way we can guarantee that the correct error will be returned to the client.
///
/// The layer that precedes this one is responsible for handling the error and returning the correct response.
/// It will ALWAYS be able to downcast the error to the correct type.
///
pub(crate) struct RequestBodyLimitLayer<Body> {
    _phantom: std::marker::PhantomData<Body>,
    initial_limit: usize,
}
impl<Body> RequestBodyLimitLayer<Body> {
    pub(crate) fn new(initial_limit: usize) -> Self {
        Self {
            _phantom: Default::default(),
            initial_limit,
        }
    }
}

impl<Body, S> Layer<S> for RequestBodyLimitLayer<Body>
where
    S: Service<http::request::Request<super::limited::Limited<Body>>>,
    Body: http_body::Body,
{
    type Service = RequestBodyLimit<Body, S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestBodyLimit::new(inner, self.initial_limit)
    }
}

pub(crate) struct RequestBodyLimit<Body, S> {
    _phantom: std::marker::PhantomData<Body>,
    inner: S,
    initial_limit: usize,
}

impl<Body, S: Clone> Clone for RequestBodyLimit<Body, S> {
    fn clone(&self) -> Self {
        Self {
            _phantom: std::marker::PhantomData,
            inner: self.inner.clone(),
            initial_limit: self.initial_limit,
        }
    }
}

impl<Body, S> RequestBodyLimit<Body, S>
where
    S: Service<http::request::Request<super::limited::Limited<Body>>>,
    Body: http_body::Body,
{
    fn new(inner: S, initial_limit: usize) -> Self {
        Self {
            _phantom: Default::default(),
            inner,
            initial_limit,
        }
    }
}

impl<ReqBody, RespBody, S> Service<http::Request<ReqBody>> for RequestBodyLimit<ReqBody, S>
where
    S: Service<
            http::Request<super::limited::Limited<ReqBody>>,
            Response = http::Response<RespBody>,
        >,
    ReqBody: http_body::Body,
    RespBody: http_body::Body,
    S::Error: From<RequestSizeLimitError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<ReqBody>) -> Self::Future {
        let control = BodyLimitControl::new(self.initial_limit);

        // GraphQL-over-HTTP GET requests carry the query in the URI's query string rather than
        // the body, so the content-length/body checks below never see it. Enforce the same limit
        // here to prevent bypassing it by sending a GET instead of a POST.
        //
        // NB: this re-derives the same "is this a GraphQL-over-HTTP GET" check that
        // `services::router::service::RouterToSupergraphRequestService::get_graphql_request` uses
        // to decide whether to parse the query from the URI vs. the body. If that convention ever
        // changes, update this check too.
        if req.method() == http::Method::GET {
            let query_len = req.uri().query().map(str::len).unwrap_or(0);
            if query_len > control.limit() {
                return ResponseFuture::Reject {
                    error: RequestSizeLimitError::QueryTooLarge,
                };
            }
        }

        let content_length = req
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());

        let _body_limit = match content_length {
            Some(len) if len > control.limit() => {
                return ResponseFuture::Reject {
                    error: RequestSizeLimitError::BodyTooLarge,
                };
            }
            Some(len) => control.limit().min(len),
            None => control.limit(),
        };

        // TODO: We can only do this once this layer is moved to the beginning of the router pipeline.
        // Otherwise the context length will be checked against the decompressed size of the body.
        // self.control.update_limit(_body_limit);

        // This mutex allows us to signal the body stream to stop processing if the limit is hit.
        let abort = Arc::new(tokio::sync::Semaphore::new(1));

        // This will be dropped if the body stream hits the limit signalling an immediate response.
        let owned_permit = abort
            .clone()
            .try_acquire_owned()
            .expect("abort lock is new, qed");

        // Add the body limit to the request extensions
        req.extensions_mut().insert(control.clone());

        let f = self
            .inner
            .call(req.map(|body| super::limited::Limited::new(body, control, owned_permit)));

        ResponseFuture::Continue {
            inner: f,
            abort: abort.acquire_owned().boxed(),
        }
    }
}

pin_project! {
    #[project = ResponseFutureProj]
    pub (crate) enum ResponseFuture<F> {
        Reject {
            error: RequestSizeLimitError,
        },
        Continue {
            #[pin]
            inner: F,

            #[pin]
            abort: futures::future::BoxFuture<'static, Result<OwnedSemaphorePermit, AcquireError>>,
        }
    }
}

impl<Inner, Body, Error> Future for ResponseFuture<Inner>
where
    Inner: Future<Output = Result<http::response::Response<Body>, Error>>,
    Body: http_body::Body,
    Error: From<RequestSizeLimitError>,
{
    type Output = Result<http::response::Response<Body>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let project = self.project();
        match project {
            // Eager reject: either the content-length header or the GET query string exceeded the limit
            ResponseFutureProj::Reject { error } => Poll::Ready(Err((*error).into())),
            // Continue processing the request
            ResponseFutureProj::Continue { inner, abort, .. } => {
                match inner.poll(cx) {
                    Poll::Ready(r) => Poll::Ready(r),
                    Poll::Pending => {
                        // Check to see if the stream limit has been hit
                        match abort.poll(cx) {
                            Poll::Ready(_) => {
                                Poll::Ready(Err(RequestSizeLimitError::BodyTooLarge.into()))
                            }
                            Poll::Pending => Poll::Pending,
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::future::Future;

    use futures::stream::StreamExt;
    use http::StatusCode;
    use http_body_util::BodyStream;
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt;
    use tower_service::Service;

    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::plugins::limits::layer::RequestBodyLimitLayer;
    use crate::plugins::limits::limited::Limited;

    /// Builds a `RequestBodyLimitLayer`-wrapped service with the given `limit` and inner handler.
    fn service_with_limit<F, Fut>(
        limit: usize,
        inner: F,
    ) -> impl Service<http::Request<String>, Response = http::Response<String>, Error = BoxError>
    where
        F: FnMut(http::Request<Limited<String>>) -> Fut,
        Fut: Future<Output = Result<http::Response<String>, BoxError>>,
    {
        ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(limit))
            .service_fn(inner)
    }

    /// The inner service should never complete: either it's never called at all (eager rejection
    /// on method/header) or the body stream stalls once the limit is hit while reading.
    async fn unreachable_after_reading_body(
        r: http::Request<Limited<String>>,
    ) -> Result<http::Response<String>, BoxError> {
        BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
        panic!("inner service should not have completed");
    }

    async fn ok_after_reading_body(
        r: http::Request<Limited<String>>,
    ) -> Result<http::Response<String>, BoxError> {
        BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
        Ok(http::Response::builder()
            .status(StatusCode::OK)
            .body("This is a test".to_string())
            .unwrap())
    }

    fn assert_ok(resp: Result<http::Response<String>, BoxError>) {
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "This is a test");
    }

    /// Readies `service` and calls it with `req`, panicking if the service is never ready.
    async fn call<S>(
        service: &mut S,
        req: http::Request<String>,
    ) -> Result<http::Response<String>, BoxError>
    where
        S: Service<http::Request<String>, Response = http::Response<String>, Error = BoxError>,
    {
        service.ready().await.unwrap().call(req).await
    }

    #[tokio::test]
    async fn test_body_content_length_limit_exceeded() {
        let mut service = service_with_limit(10, unreachable_after_reading_body);
        let request = http::Request::new("This is a test".to_string());
        let resp = call(&mut service, request).await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_body_content_length_limit_ok() {
        let mut service = service_with_limit(10, ok_after_reading_body);
        let resp = call(&mut service, http::Request::new("OK".to_string())).await;
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_header_content_length_limit_exceeded() {
        let mut service = service_with_limit(10, unreachable_after_reading_body);
        let request = http::Request::builder()
            .header("Content-Length", "100")
            .body("This is a test".to_string())
            .unwrap();
        let resp = call(&mut service, request).await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_header_content_length_limit_ok() {
        let mut service = service_with_limit(10, ok_after_reading_body);
        let request = http::Request::builder()
            .header("Content-Length", "5")
            .body("OK".to_string())
            .unwrap();
        let resp = call(&mut service, request).await;
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_get_query_length_limit_exceeded() {
        let mut service = service_with_limit(10, unreachable_after_reading_body);
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("/graphql?query=this-query-string-is-way-longer-than-the-limit")
            .body(String::new())
            .unwrap();
        let err = call(&mut service, request).await.unwrap_err();
        assert!(matches!(
            err.downcast_ref::<super::RequestSizeLimitError>(),
            Some(super::RequestSizeLimitError::QueryTooLarge)
        ));
    }

    #[tokio::test]
    async fn test_get_query_length_limit_ok() {
        let mut service = service_with_limit(10, ok_after_reading_body);
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("/graphql?query=ok")
            .body(String::new())
            .unwrap();
        let resp = call(&mut service, request).await;
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_get_query_length_exactly_at_limit_ok() {
        let mut service = service_with_limit(10, ok_after_reading_body);
        // "query=1234" is exactly 10 bytes, matching the configured limit.
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("/graphql?query=1234")
            .body(String::new())
            .unwrap();
        let resp = call(&mut service, request).await;
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_get_query_length_one_byte_over_limit_exceeded() {
        let mut service = service_with_limit(10, unreachable_after_reading_body);
        // "query=12345" is exactly 11 bytes, one over the configured limit of 10.
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("/graphql?query=12345")
            .body(String::new())
            .unwrap();
        let err = call(&mut service, request).await.unwrap_err();
        assert!(matches!(
            err.downcast_ref::<super::RequestSizeLimitError>(),
            Some(super::RequestSizeLimitError::QueryTooLarge)
        ));
    }

    #[tokio::test]
    async fn test_get_request_without_query_string_not_rejected() {
        // `req.uri().query()` is `None` for a bare GET (e.g. a health-check route), exercising
        // the `unwrap_or(0)` fallback rather than a GET request that always supplies `?query=...`.
        let mut service = service_with_limit(10, ok_after_reading_body);
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("/health")
            .body(String::new())
            .unwrap();
        let resp = call(&mut service, request).await;
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_post_request_query_string_not_checked() {
        // The GET query-length check must not affect POST requests, even if their (unused)
        // query string would exceed the limit; only the body/content-length matters for POST.
        let mut service = service_with_limit(10, ok_after_reading_body);
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("/graphql?query=this-query-string-is-way-longer-than-the-limit")
            .body("OK".to_string())
            .unwrap();
        let resp = call(&mut service, request).await;
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_limits_dynamic_update() {
        let mut service = service_with_limit(10, move |r: http::Request<Limited<String>>| {
            //Update the limit before we start reading the stream
            r.extensions()
                .get::<BodyLimitControl>()
                .expect("cody limit must have been added to extensions")
                .update_limit(100);
            async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            }
        });
        let request = http::Request::new("This is a test".to_string());
        let resp = call(&mut service, request).await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn test_body_length_exceeds_content_length() {
        let mut service = service_with_limit(10, ok_after_reading_body);
        let request = http::Request::builder()
            .header("Content-Length", "5")
            .body("Exceeded".to_string())
            .unwrap();
        let resp = call(&mut service, request).await;
        //TODO this needs to to fail once the limit layer is moved before decompression.
        assert_ok(resp);
    }

    #[tokio::test]
    async fn test_body_content_length_service_reuse() {
        let mut service = service_with_limit(10, ok_after_reading_body);

        for _ in 0..10 {
            let resp = call(&mut service, http::Request::new("OK".to_string())).await;
            assert_ok(resp);
        }
    }
}
