//! The layers that decide whether a request to a subgraph or connector source goes ahead.
//!
//! Admission has two parts, from the outside in: the error responses and the rate limit. The rate
//! limit is charged where a request is called and refuses a request it does not admit with
//! [`Overloaded`], its counter shared by every clone of the built service (`rate_limit`); the
//! error responses, above it, turn that error into a GraphQL response. No buffer stands between
//! them or above them: the stack's pool, above the plugin hooks' layers, readies and calls it on
//! the request's task.
//!
//! The subgraph and connector request stages place each part separately. Their timeouts answer
//! their own errors.

use apollo_federation::connectors::runtime::errors::Error;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::load_shed::error::Overloaded;
use tower::util::BoxCloneService;

use super::rate_limit_error;
use crate::layers::ServiceBuilderExt as _;
use crate::services::SubgraphResponse;
use crate::services::connector::request_service;
use crate::services::subgraph;

/// Layer type for [`TrafficShaping::subgraph_error_response_layer`](super::TrafficShaping::subgraph_error_response_layer).
pub(crate) struct SubgraphErrorResponseLayer;

impl<S> Layer<S> for SubgraphErrorResponseLayer
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = BoxCloneService<subgraph::Request, subgraph::Response, BoxError>;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &subgraph::Request| (req.context.clone(), req.subgraph_name.clone()),
                |(ctx, subgraph_name), future| async {
                    let response: Result<SubgraphResponse, BoxError> = future.await;
                    match response {
                        Err(err) if err.is::<Overloaded>() => {
                            // TODO add metrics
                            Ok(SubgraphResponse::error_builder()
                                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                .subgraph_name(subgraph_name)
                                .error(rate_limit_error())
                                .context(ctx)
                                .build())
                        }
                        _ => response,
                    }
                },
            )
            .service(inner)
            .boxed_clone()
    }
}

/// Layer type for [`TrafficShaping::connector_source_error_response_layer`](super::TrafficShaping::connector_source_error_response_layer).
pub(crate) struct ConnectorSourceErrorResponseLayer;

impl<S> Layer<S> for ConnectorSourceErrorResponseLayer
where
    S: Service<request_service::Request, Response = request_service::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = BoxCloneService<request_service::Request, request_service::Response, BoxError>;

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
                        Err(err) if err.is::<Overloaded>() => {
                            Ok(request_service::Response::error_new(
                                context,
                                subgraph_name,
                                Error::RateLimited,
                                "Your request has been rate limited",
                                response_key,
                            ))
                        }
                        _ => response,
                    }
                },
            )
            .service(inner)
            .boxed_clone()
    }
}

#[cfg(test)]
mod tests {
    use apollo_federation::connectors::runtime::errors::Error;
    use http::StatusCode;
    use tower::BoxError;
    use tower::Layer;
    use tower::ServiceExt;
    use tower::load_shed::error::Overloaded;
    use tower::timeout::error::Elapsed;

    use super::ConnectorSourceErrorResponseLayer;
    use super::SubgraphErrorResponseLayer;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::connector::request_service;
    use crate::services::connector::request_service::TransportOutcome;

    /// The response the error-response layer gives when the service beneath it fails with `error`.
    async fn respond_to(error: BoxError) -> Result<SubgraphResponse, BoxError> {
        // shared, so the service is cloneable as the layer requires
        let error = std::sync::Arc::new(parking_lot::Mutex::new(Some(error)));
        let failing = tower::service_fn(move |_: SubgraphRequest| {
            let error = error.lock().take().expect("called once");
            async move { Err::<SubgraphResponse, _>(error) }
        });
        SubgraphErrorResponseLayer
            .layer(failing)
            .oneshot(
                SubgraphRequest::fake_builder()
                    .subgraph_name("products")
                    .build(),
            )
            .await
    }

    fn error_code(response: &SubgraphResponse) -> Option<String> {
        response.response.body().errors.first()?.extension_code()
    }

    #[tokio::test]
    async fn overload_becomes_rate_limited() {
        let response = respond_to(Overloaded::new().into()).await.unwrap();
        assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            error_code(&response).as_deref(),
            Some("REQUEST_RATE_LIMITED")
        );
    }

    #[tokio::test]
    async fn other_errors_pass_through() {
        let error = respond_to("connection refused".into()).await.unwrap_err();
        assert_eq!(error.to_string(), "connection refused");
        // The subgraph timeout answers its own errors.
        let error = respond_to(Elapsed::new().into()).await.unwrap_err();
        assert!(error.is::<Elapsed>());
    }

    /// The response the connector source error-response layer gives when the service beneath it
    /// fails with `error`.
    async fn source_responds_to(error: BoxError) -> Result<request_service::Response, BoxError> {
        // shared, so the service is cloneable as the layer requires
        let error = std::sync::Arc::new(parking_lot::Mutex::new(Some(error)));
        let failing = tower::service_fn(move |_: request_service::Request| {
            let error = error.lock().take().expect("called once");
            async move { Err::<request_service::Response, _>(error) }
        });
        ConnectorSourceErrorResponseLayer
            .layer(failing)
            .oneshot(request_service::Request::test_new())
            .await
    }

    #[tokio::test]
    async fn source_overload_becomes_rate_limited() {
        let response = source_responds_to(Overloaded::new().into()).await.unwrap();
        assert!(matches!(
            response.transport_outcome,
            TransportOutcome::Error(Error::RateLimited)
        ));
    }

    #[tokio::test]
    async fn other_source_errors_pass_through() {
        let error = source_responds_to("connection refused".into())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "connection refused");
        // The connector source timeout answers its own errors.
        let error = source_responds_to(Elapsed::new().into()).await.unwrap_err();
        assert!(error.is::<Elapsed>());
    }
}
