use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;
use tower::Layer;
use tower_service::Service;

use super::guard::SubgraphRequestGuard;
use super::tracker::RouterOverheadTracker;
use crate::services::http::{HttpRequest, HttpResponse};

/// Tower layer that tracks router overhead by creating guards for HTTP client requests.
///
/// This layer extracts the RouterOverheadTracker from the request context and creates
/// a SubgraphRequestGuard that lives for the duration of the HTTP request.
#[derive(Clone)]
pub(crate) struct OverheadLayer;

impl OverheadLayer {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for OverheadLayer {
    type Service = OverheadService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OverheadService { inner }
    }
}

/// Service that creates overhead tracking guards for each HTTP request.
pub(crate) struct OverheadService<S> {
    inner: S,
}

impl<S> Clone for OverheadService<S>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<S> Service<HttpRequest> for OverheadService<S>
where
    S: Service<HttpRequest, Response = HttpResponse> + Send,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = S::Error;
    type Future = OverheadFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        // Try to extract the RouterOverheadTracker from the router request context
        // Note: The tracker is stored in the router-level context, not the HTTP request context
        let guard = request
            .context
            .extensions()
            .with_lock(|lock| lock.get::<RouterOverheadTracker>().cloned())
            .map(|tracker| tracker.create_guard());

        let future = self.inner.call(request);

        OverheadFuture {
            inner: future,
            _guard: guard,
        }
    }
}

pin_project! {
    /// Future that holds a SubgraphRequestGuard for the duration of the HTTP request.
    ///
    /// When this future is dropped (either after completion or due to cancellation),
    /// the guard is also dropped, which properly updates the overhead tracking.
    pub(crate) struct OverheadFuture<F> {
        #[pin]
        inner: F,
        // The guard is held here and will be dropped when this future is dropped
        _guard: Option<SubgraphRequestGuard>,
    }
}

impl<F, E> Future for OverheadFuture<F>
where
    F: Future<Output = Result<HttpResponse, E>>,
{
    type Output = Result<HttpResponse, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tower::{Service, ServiceBuilder, ServiceExt};

    use super::*;
    use crate::Context;
    use crate::services::http::{HttpRequest, HttpResponse};
    use crate::services::router::body;

    // Mock service for testing
    #[derive(Clone)]
    struct MockHttpService;

    impl Service<HttpRequest> for MockHttpService {
        type Response = HttpResponse;
        type Error = tower::BoxError;
        type Future = std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
        >;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: HttpRequest) -> Self::Future {
            Box::pin(async move {
                Ok(HttpResponse {
                    http_response: http::Response::new(body::empty()),
                    context: req.context,
                })
            })
        }
    }

    #[tokio::test]
    async fn test_layer_creates_guard_when_tracker_present() {
        let tracker = RouterOverheadTracker::new();
        let context = Context::new();

        // Store tracker in context
        context.extensions().with_lock(|lock| {
            lock.insert(tracker.clone());
        });

        // Create the service with the layer
        let mut service = ServiceBuilder::new()
            .layer(OverheadLayer::new())
            .service(MockHttpService);

        // Create a request
        let request = HttpRequest {
            http_request: http::Request::new(body::empty()),
            context: context.clone(),
        };

        // Wait a bit before the request
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Make the request
        let _response = service.ready().await.unwrap().call(request).await.unwrap();

        // Wait a bit after the request
        tokio::time::sleep(Duration::from_millis(10)).await;

        // The overhead should be approximately the time we waited (2 x 10ms = 20ms)
        // Allow generous tolerance for timing imprecision in tests
        let result = tracker.calculate_overhead();
        // The guard should have been dropped when the future completed
        assert!(!result.has_active_subgraph_requests);
        assert!(result.overhead >= Duration::from_millis(10) && result.overhead <= Duration::from_millis(60),
            "overhead was {:?}", result.overhead);
    }

    #[tokio::test]
    async fn test_layer_works_without_tracker() {
        let context = Context::new();

        // Create the service with the layer (no tracker in context)
        let mut service = ServiceBuilder::new()
            .layer(OverheadLayer::new())
            .service(MockHttpService);

        // Create a request
        let request = HttpRequest {
            http_request: http::Request::new(body::empty()),
            context,
        };

        // Should not panic even without a tracker
        let _response = service.ready().await.unwrap().call(request).await.unwrap();
    }
}