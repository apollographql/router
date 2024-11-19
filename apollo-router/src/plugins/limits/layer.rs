use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
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

struct BodyLimitControlInner {
    limit: AtomicUsize,
    current: AtomicUsize,
}

impl Clone for BodyLimitControlInner {
    fn clone(&self) -> Self {
        Self {
            limit: AtomicUsize::new(self.limit.load(std::sync::atomic::Ordering::SeqCst)),
            current: AtomicUsize::new(0),
        }
    }
}

/// This structure allows the body limit to be updated dynamically.
/// It also allows the error message to be updated
#[derive(Clone)]
pub(crate) struct BodyLimitControl {
    inner: BodyLimitControlInner,
}

impl BodyLimitControl {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            inner: BodyLimitControlInner {
                limit: AtomicUsize::new(limit),
                current: AtomicUsize::new(0),
            },
        }
    }

    /// To disable the limit check just set this to usize::MAX
    pub(crate) fn update_limit(&self, limit: usize) {
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
    control: BodyLimitControl,
}
impl<Body> RequestBodyLimitLayer<Body> {
    pub(crate) fn new(control: BodyLimitControl) -> Self {
        Self {
            _phantom: Default::default(),
            control,
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
        RequestBodyLimit::new(inner, self.control.clone())
    }
}

pub(crate) struct RequestBodyLimit<Body, S> {
    _phantom: std::marker::PhantomData<Body>,
    inner: S,
    control: BodyLimitControl,
}

impl<Body, S> RequestBodyLimit<Body, S>
where
    S: Service<http::request::Request<super::limited::Limited<Body>>>,
    Body: http_body::Body,
{
    fn new(inner: S, control: BodyLimitControl) -> Self {
        Self {
            _phantom: Default::default(),
            inner,
            control,
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

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let content_length = req
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());

        let _body_limit = match content_length {
            Some(len) if len > self.control.limit() => return ResponseFuture::Reject,
            Some(len) => self.control.limit().min(len),
            None => self.control.limit(),
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

        let f =
            self.inner.call(req.map(|body| {
                super::limited::Limited::new(body, self.control.clone(), owned_permit)
            }));

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
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower_service::Service;

    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::plugins::limits::layer::RequestBodyLimitLayer;
    use crate::services;

    #[tokio::test]
    async fn test_body_content_length_limit_exceeded() {
        let control = BodyLimitControl::new(10);
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(control.clone()))
            .service_fn(|r: http::Request<_>| async move {
                services::http::body_stream::BodyStream::new(r.into_body())
                    .collect::<Vec<_>>()
                    .await;
                panic!("should have rejected request");
            });
        let resp: Result<http::Response<String>, BoxError> = service
            .call(http::Request::new("This is a test".to_string()))
            .await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_body_content_length_limit_ok() {
        let control = BodyLimitControl::new(10);
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(control.clone()))
            .service_fn(|r: http::Request<_>| async move {
                services::http::body_stream::BodyStream::new(r.into_body())
                    .collect::<Vec<_>>()
                    .await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });
        let resp: Result<_, BoxError> = service.call(http::Request::new("OK".to_string())).await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "This is a test");
    }

    #[tokio::test]
    async fn test_header_content_length_limit_exceeded() {
        let control = BodyLimitControl::new(10);
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(control.clone()))
            .service_fn(|r: http::Request<_>| async move {
                services::http::body_stream::BodyStream::new(r.into_body())
                    .collect::<Vec<_>>()
                    .await;
                panic!("should have rejected request");
            });
        let resp: Result<http::Response<String>, BoxError> = service
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
        let control = BodyLimitControl::new(10);
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(control.clone()))
            .service_fn(|r: http::Request<_>| async move {
                services::http::body_stream::BodyStream::new(r.into_body())
                    .collect::<Vec<_>>()
                    .await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });
        let resp: Result<_, BoxError> = service
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
        let control = BodyLimitControl::new(10);
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(control.clone()))
            .service_fn(move |r: http::Request<_>| {
                let control = control.clone();
                async move {
                    services::http::body_stream::BodyStream::new(r.into_body())
                        .collect::<Vec<_>>()
                        .await;
                    control.update_limit(100);
                    Ok(http::Response::builder()
                        .status(StatusCode::OK)
                        .body("This is a test".to_string())
                        .unwrap())
                }
            });
        let resp: Result<_, BoxError> = service
            .call(http::Request::new("This is a test".to_string()))
            .await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_body_length_exceeds_content_length() {
        let control = BodyLimitControl::new(10);
        let mut service = ServiceBuilder::new()
            .layer(RequestBodyLimitLayer::new(control.clone()))
            .service_fn(|r: http::Request<_>| async move {
                services::http::body_stream::BodyStream::new(r.into_body())
                    .collect::<Vec<_>>()
                    .await;
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("This is a test".to_string())
                    .unwrap())
            });
        let resp: Result<_, BoxError> = service
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
}
