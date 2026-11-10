//! The router stage's, the subgraph and the connector source timeouts, which answer a request that
//! runs too long themselves.
//!
//! Each timeout turns its own [`Elapsed`] error into a gateway-timeout response, so it can be
//! placed anywhere in the stack: no layer above it needs to know about the error.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::task::ready;
use std::time::Duration;

use apollo_federation::connectors::runtime::errors::Error;
use http::StatusCode;
use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::load_shed::error::Overloaded;
use tower::timeout::TimeoutLayer;
use tower::timeout::error::Elapsed;

use super::gateway_timeout_error;
use super::rate_limit_error;
use crate::layers::ServiceBuilderExt as _;
use crate::layers::release_readied;
use crate::services::SubgraphResponse;
use crate::services::connector::request_service;
use crate::services::router;
use crate::services::subgraph;

/// Layer type for [`TrafficShaping::subgraph_timeout_layer`](super::TrafficShaping::subgraph_timeout_layer).
pub(crate) struct SubgraphTimeoutLayer {
    timeout: Duration,
}

impl SubgraphTimeoutLayer {
    pub(super) fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl<S> Layer<S> for SubgraphTimeoutLayer
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = subgraph::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &subgraph::Request| (req.context.clone(), req.subgraph_name.clone()),
                |(ctx, subgraph_name), future| async {
                    let response: Result<SubgraphResponse, BoxError> = future.await;
                    match response {
                        Err(err) if err.is::<Elapsed>() => {
                            // TODO add metrics
                            Ok(SubgraphResponse::error_builder()
                                .status_code(StatusCode::GATEWAY_TIMEOUT)
                                .subgraph_name(subgraph_name)
                                .error(gateway_timeout_error())
                                .context(ctx)
                                .build())
                        }
                        _ => response,
                    }
                },
            )
            .layer(TimeoutLayer::new(self.timeout))
            .service(inner)
            .boxed_clone()
    }
}

/// Layer type for [`TrafficShaping::connector_source_timeout_layer`](super::TrafficShaping::connector_source_timeout_layer).
pub(crate) struct ConnectorSourceTimeoutLayer {
    timeout: Duration,
}

impl ConnectorSourceTimeoutLayer {
    pub(super) fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl<S> Layer<S> for ConnectorSourceTimeoutLayer
where
    S: Service<request_service::Request, Response = request_service::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = request_service::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &request_service::Request| {
                    (
                        req.context.clone(),
                        req.key.clone(),
                        req.connector.id.subgraph_name.to_string(),
                    )
                },
                |(context, response_key, subgraph_name), future| async {
                    let response: Result<request_service::Response, BoxError> = future.await;
                    match response {
                        Err(err) if err.is::<Elapsed>() => {
                            Ok(request_service::Response::error_new(
                                context,
                                subgraph_name,
                                Error::GatewayTimeout,
                                "Your request has been timed out",
                                response_key,
                            ))
                        }
                        _ => response,
                    }
                },
            )
            .layer(TimeoutLayer::new(self.timeout))
            .service(inner)
            .boxed_clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use apollo_federation::connectors::runtime::errors::Error;
    use http::StatusCode;
    use tower::BoxError;
    use tower::Layer;
    use tower::ServiceExt;

    use super::ConnectorSourceTimeoutLayer;
    use super::SubgraphTimeoutLayer;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::connector::request_service;
    use crate::services::connector::request_service::TransportOutcome;

    /// The response a 100ms timeout gives for a subgraph that answers after `delay`.
    async fn respond_after(delay: Duration) -> Result<SubgraphResponse, BoxError> {
        let slow = tower::service_fn(move |_: SubgraphRequest| async move {
            tokio::time::sleep(delay).await;
            Ok::<_, BoxError>(SubgraphResponse::fake_builder().build())
        });
        SubgraphTimeoutLayer::new(Duration::from_millis(100))
            .layer(slow)
            .oneshot(
                SubgraphRequest::fake_builder()
                    .subgraph_name("products")
                    .build(),
            )
            .await
    }

    #[tokio::test(start_paused = true)]
    async fn late_response_becomes_gateway_timeout() {
        let response = respond_after(Duration::from_secs(1)).await.unwrap();
        assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            response.response.body().errors[0]
                .extension_code()
                .as_deref(),
            Some("GATEWAY_TIMEOUT")
        );
        assert_eq!(response.subgraph_name, "products");
    }

    #[tokio::test(start_paused = true)]
    async fn response_in_time_passes_through() {
        let response = respond_after(Duration::from_millis(10)).await.unwrap();
        assert_eq!(response.response.status(), StatusCode::OK);
    }

    /// The response a 100ms timeout gives for a connector source that answers after `delay`.
    async fn source_responds_after(delay: Duration) -> Result<request_service::Response, BoxError> {
        let slow = tower::service_fn(move |req: request_service::Request| async move {
            tokio::time::sleep(delay).await;
            Ok::<_, BoxError>(request_service::Response::test_new(
                req.context,
                req.key,
                Vec::new(),
                Default::default(),
                None,
            ))
        });
        ConnectorSourceTimeoutLayer::new(Duration::from_millis(100))
            .layer(slow)
            .oneshot(request_service::Request::test_new())
            .await
    }

    #[tokio::test(start_paused = true)]
    async fn late_source_response_becomes_gateway_timeout() {
        let response = source_responds_after(Duration::from_secs(1)).await.unwrap();
        assert!(matches!(
            response.transport_outcome,
            TransportOutcome::Error(Error::GatewayTimeout)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn source_response_in_time_passes_through() {
        let response = source_responds_after(Duration::from_millis(10))
            .await
            .unwrap();
        assert!(!matches!(
            response.transport_outcome,
            TransportOutcome::Error(_)
        ));
    }
}

/// The router stage's shaping where no router limit is configured: the timeout every request runs
/// under, answered with a gateway timeout, and the shedding of a request whose service beneath is
/// not ready, answered as the buffered stage's innermost shedding answers it; an overload or a
/// timeout the service beneath answers with is answered as the buffered stage's mappings answer
/// them, the rate-limit 503 and the 504. Its readiness is read and its timeout armed on the
/// request's own task: the buffer the buffered stage puts in front of the shedding serves a limiter
/// every clone of the stage must share, and there is none.
pub(crate) struct RouterTimeoutLayer {
    timeout: Duration,
}

impl RouterTimeoutLayer {
    pub(super) fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl<S> Layer<S> for RouterTimeoutLayer {
    type Service = RouterTimeout<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RouterTimeout {
            inner,
            timeout: self.timeout,
            ready: false,
        }
    }
}

/// See [`RouterTimeoutLayer`].
pub(crate) struct RouterTimeout<S> {
    inner: S,
    timeout: Duration,
    /// Whether the service beneath was ready when last polled, as load shedding records it.
    ready: bool,
}

impl<S: Clone> Clone for RouterTimeout<S> {
    /// A clone not yet polled, as the clone of its service beneath is not readied.
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            timeout: self.timeout,
            ready: false,
        }
    }
}

impl<S> Service<router::Request> for RouterTimeout<S>
where
    S: Service<router::Request, Response = router::Response, Error = BoxError> + Clone,
{
    type Response = router::Response;
    type Error = BoxError;
    type Future = RouterTimeoutFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), BoxError>> {
        // load shedding's readiness: never pending, a request called unready is shed. The service
        // beneath is polled outside Tokio's cooperative budget, as `UnconstrainedBuffer` polls the
        // buffer behind the buffered stage's shedding: a budget-aware primitive beneath, such as a
        // plugin's concurrency limit, answers `Pending` once the request's task has spent its
        // budget, and that `Pending` would be taken for a service not ready and shed with a 503
        let polled = std::pin::pin!(tokio::task::unconstrained(std::future::poll_fn(|cx| {
            self.inner.poll_ready(cx)
        })))
        .poll(cx);
        self.ready = match polled {
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            polled => polled.is_ready(),
        };
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: router::Request) -> Self::Future {
        if !std::mem::take(&mut self.ready) {
            // the service beneath, polled and not ready, may hold a reservation it waits on
            release_readied(&mut self.inner);
            return RouterTimeoutFuture::Shed {
                context: Some(request.context),
            };
        }
        let context = request.context.clone();
        RouterTimeoutFuture::Called {
            inner: tokio::time::timeout(self.timeout, self.inner.call(request)),
            context: Some(context),
        }
    }
}

pin_project! {
    /// The future of a request through [`RouterTimeout`].
    #[project = RouterTimeoutProjection]
    pub(crate) enum RouterTimeoutFuture<F> {
        Called {
            #[pin]
            inner: tokio::time::Timeout<F>,
            context: Option<crate::Context>,
        },
        Shed {
            context: Option<crate::Context>,
        },
    }
}

impl<F> Future for RouterTimeoutFuture<F>
where
    F: Future<Output = Result<router::Response, BoxError>>,
{
    type Output = Result<router::Response, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            RouterTimeoutProjection::Called { inner, context } => {
                let mut context = || context.take().expect("a future polled after completion");
                Poll::Ready(match ready!(inner.poll(cx)) {
                    Ok(Ok(response)) => Ok(response),
                    // an overload beneath, as the buffered stage's innermost mapping answers it
                    Ok(Err(error)) if error.is::<Overloaded>() => {
                        Ok(router::Response::error_builder()
                            .status_code(StatusCode::SERVICE_UNAVAILABLE)
                            .error(rate_limit_error())
                            .context(context())
                            .build()
                            .expect("overloaded response should build"))
                    }
                    Ok(Err(error)) if !error.is::<Elapsed>() => Err(error),
                    // this timer's expiry, or a timeout beneath, as the outermost mapping answers it
                    Ok(Err(_)) | Err(_) => Ok(router::Response::error_builder()
                        .status_code(StatusCode::GATEWAY_TIMEOUT)
                        .error(gateway_timeout_error())
                        .context(context())
                        .build()
                        .expect("gateway timeout response should build")),
                })
            }
            RouterTimeoutProjection::Shed { context } => {
                Poll::Ready(Ok(router::Response::error_builder()
                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                    .error(rate_limit_error())
                    .context(context.take().expect("a future polled after completion"))
                    .build()
                    .expect("overloaded response should build")))
            }
        }
    }
}
