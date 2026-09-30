//! The layers that decide whether a request to a subgraph or connector source goes ahead.
//!
//! Admission has four parts, from the outside in: a buffer, the error responses, load shedding
//! and the rate limit. Their order matters:
//!
//! - Load shedding must sit directly above the rate limit, with no buffer between them, so it
//!   rejects a request as soon as the limit is reached instead of queueing it.
//! - The buffer above load shedding polls readiness unconstrained, so Tokio's cooperative
//!   scheduling is not mistaken for overload. It also lets the layers above clone the service
//!   while every clone shares one rate limit.
//! - The error responses must sit above load shedding: they turn its [`Overloaded`] error into
//!   a GraphQL response. This can't move into the rate limit, which only waits; it is load
//!   shedding that turns the wait into an error.
//!
//! The subgraph and connector request stages place each part separately. The subgraph timeout
//! answers its own errors; the connector source error responses also answer timeouts.

use apollo_federation::connectors::runtime::errors::Error;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::load_shed::error::Overloaded;
use tower::timeout::error::Elapsed;
use tower::util::BoxService;

use super::rate_limit_error;
use crate::layers::ServiceBuilderExt as _;
use crate::services::SubgraphResponse;
use crate::services::connector::request_service;
use crate::services::subgraph;

/// Layer type for [`TrafficShaping::subgraph_error_response_layer`](super::TrafficShaping::subgraph_error_response_layer).
pub(crate) struct SubgraphErrorResponseLayer;

impl<S> Layer<S> for SubgraphErrorResponseLayer
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError> + Send + 'static,
    S::Future: Send + 'static,
{
    type Service = BoxService<subgraph::Request, subgraph::Response, BoxError>;

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
            .boxed()
    }
}

/// Layer type for [`TrafficShaping::connector_source_error_response_layer`](super::TrafficShaping::connector_source_error_response_layer).
pub(crate) struct ConnectorSourceErrorResponseLayer;

impl<S> Layer<S> for ConnectorSourceErrorResponseLayer
where
    S: Service<request_service::Request, Response = request_service::Response, Error = BoxError>
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = BoxService<request_service::Request, request_service::Response, BoxError>;

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
                        Ok(ok) => Ok(ok),
                        Err(err) if err.is::<Elapsed>() => {
                            Ok(request_service::Response::error_new(
                                context,
                                subgraph_name,
                                Error::GatewayTimeout,
                                "Your request has been timed out",
                                response_key,
                            ))
                        }
                        Err(err) if err.is::<Overloaded>() => {
                            Ok(request_service::Response::error_new(
                                context,
                                subgraph_name,
                                Error::RateLimited,
                                "Your request has been rate limited",
                                response_key,
                            ))
                        }
                        Err(err) => Err(err),
                    }
                },
            )
            .service(inner)
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use tower::BoxError;
    use tower::Layer;
    use tower::ServiceExt;
    use tower::load_shed::error::Overloaded;
    use tower::timeout::error::Elapsed;

    use super::SubgraphErrorResponseLayer;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    /// The response the error-response layer gives when the service beneath it fails with `error`.
    async fn respond_to(error: BoxError) -> Result<SubgraphResponse, BoxError> {
        let mut error = Some(error);
        let failing = tower::service_fn(move |_: SubgraphRequest| {
            let error = error.take().expect("called once");
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
}
