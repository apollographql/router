//! The subgraph and connector source timeouts, which answer a request that runs too long
//! themselves.
//!
//! Each timeout turns its own [`Elapsed`] error into a gateway-timeout response, so it can be
//! placed anywhere in the stack: no layer above it needs to know about the error.

use std::time::Duration;

use apollo_federation::connectors::runtime::errors::Error;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::timeout::TimeoutLayer;
use tower::timeout::error::Elapsed;

use super::gateway_timeout_error;
use crate::layers::ServiceBuilderExt as _;
use crate::services::SubgraphResponse;
use crate::services::connector::request_service;
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
