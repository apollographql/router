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

#[derive(thiserror::Error, Debug, Display)]
pub(super) enum BodyLimitError {
    /// Request body payload too large
    PayloadTooLarge,
}

#[derive(thiserror::Error, Debug, Display)]
pub(super) enum HeaderLimitError {
    /// Request header fields too many
    TooManyHeaders,
    /// Request header list too many items
    TooManyHeaderListItems,
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
    S::Error: From<BodyLimitError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<ReqBody>) -> Self::Future {
        let control = BodyLimitControl::new(self.initial_limit);
        let content_length = req
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());

        let _body_limit = match content_length {
            Some(len) if len > control.limit() => return ResponseFuture::Reject,
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
        Reject,
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
    Error: From<BodyLimitError>,
{
    type Output = Result<http::response::Response<Body>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let project = self.project();
        match project {
            // Content-length header exceeded, eager reject
            ResponseFutureProj::Reject => Poll::Ready(Err(BodyLimitError::PayloadTooLarge.into())),
            // Continue processing the request
            ResponseFutureProj::Continue { inner, abort, .. } => {
                match inner.poll(cx) {
                    Poll::Ready(r) => Poll::Ready(r),
                    Poll::Pending => {
                        // Check to see if the stream limit has been hit
                        match abort.poll(cx) {
                            Poll::Ready(_) => {
                                Poll::Ready(Err(BodyLimitError::PayloadTooLarge.into()))
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
    use futures::stream::StreamExt;
    use http::StatusCode;
    use http_body_util::BodyStream;
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt;
    use tower_service::Service;

    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::plugins::limits::layer::RequestBodyLimitLayer;

    #[tokio::test]
    async fn test_body_content_length_limit_exceeded() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(|r: http::Request<_>| async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                panic!("should have rejected request");
            });
        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(http::Request::new("This is a test".to_string()))
            .await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_body_content_length_limit_ok() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(|r: http::Request<_>| async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });
        let resp: Result<_, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(http::Request::new("OK".to_string()))
            .await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "This is a test");
    }

    #[tokio::test]
    async fn test_header_content_length_limit_exceeded() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(|r: http::Request<_>| async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                panic!("should have rejected request");
            });
        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("Content-Length", "100")
                    .body("This is a test".to_string())
                    .unwrap(),
            )
            .await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_header_content_length_limit_ok() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(|r: http::Request<_>| async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });
        let resp: Result<_, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("Content-Length", "5")
                    .body("OK".to_string())
                    .unwrap(),
            )
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "This is a test");
    }

    #[tokio::test]
    async fn test_limits_dynamic_update() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(move |r: http::Request<_>| {
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
        let resp: Result<_, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(http::Request::new("This is a test".to_string()))
            .await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn test_body_length_exceeds_content_length() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(|r: http::Request<_>| async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });
        let resp: Result<_, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("Content-Length", "5")
                    .body("Exceeded".to_string())
                    .unwrap(),
            )
            .await;
        assert!(resp.is_ok());
        //TODO this needs to to fail once the limit layer is moved before decompression.
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "This is a test");
    }

    #[tokio::test]
    async fn test_body_content_length_service_reuse() {
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(10))
            .service_fn(|r: http::Request<_>| async move {
                BodyStream::new(r.into_body()).collect::<Vec<_>>().await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });

        for _ in 0..10 {
            let resp: Result<_, BoxError> = service
                .ready()
                .await
                .unwrap()
                .call(http::Request::new("OK".to_string()))
                .await;

            assert!(resp.is_ok());
            let resp = resp.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(resp.into_body(), "This is a test");
        }
    }
}

/// Layer that limits the number of headers in an HTTP request
pub(crate) struct RequestHeaderCountLimitLayer {
    max_headers: Option<usize>,
}

impl RequestHeaderCountLimitLayer {
    pub(crate) fn new(max_headers: Option<usize>) -> Self {
        Self { max_headers }
    }
}

impl<S> Layer<S> for RequestHeaderCountLimitLayer {
    type Service = RequestHeaderCountLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestHeaderCountLimit::new(inner, self.max_headers)
    }
}

pub(crate) struct RequestHeaderCountLimit<S> {
    inner: S,
    max_headers: Option<usize>,
}

impl<S> RequestHeaderCountLimit<S> {
    fn new(inner: S, max_headers: Option<usize>) -> Self {
        Self { inner, max_headers }
    }
}

impl<ReqBody, RespBody, S> Service<http::Request<ReqBody>> for RequestHeaderCountLimit<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<RespBody>>,
    S::Error: From<HeaderLimitError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures::future::Either<futures::future::Ready<Result<S::Response, S::Error>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        if let Some(max_headers) = self.max_headers {
            if req.headers().len() > max_headers {
                // This will be converted to the appropriate response by the error handling
                return futures::future::Either::Left(futures::future::ready(Err(HeaderLimitError::TooManyHeaders.into())));
            }
        }
        futures::future::Either::Right(self.inner.call(req))
    }
}

/// Layer that limits the number of items in header lists (for headers with multiple values)
pub(crate) struct RequestHeaderListItemsLimitLayer {
    max_items: Option<usize>,
}

impl RequestHeaderListItemsLimitLayer {
    pub(crate) fn new(max_items: Option<usize>) -> Self {
        Self { max_items }
    }
}

impl<S> Layer<S> for RequestHeaderListItemsLimitLayer {
    type Service = RequestHeaderListItemsLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestHeaderListItemsLimit::new(inner, self.max_items)
    }
}

pub(crate) struct RequestHeaderListItemsLimit<S> {
    inner: S,
    max_items: Option<usize>,
}

impl<S> RequestHeaderListItemsLimit<S> {
    fn new(inner: S, max_items: Option<usize>) -> Self {
        Self { inner, max_items }
    }
}

impl<ReqBody, RespBody, S> Service<http::Request<ReqBody>> for RequestHeaderListItemsLimit<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<RespBody>>,
    S::Error: From<HeaderLimitError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures::future::Either<futures::future::Ready<Result<S::Response, S::Error>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        if let Some(max_items) = self.max_items {
            for (header_name, _) in req.headers().iter() {
                let header_values_count = req.headers().get_all(header_name).iter().count();
                if header_values_count > max_items {
                    return futures::future::Either::Left(futures::future::ready(Err(HeaderLimitError::TooManyHeaderListItems.into())));
                }
            }
        }
        futures::future::Either::Right(self.inner.call(req))
    }
}
