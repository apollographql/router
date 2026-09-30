//! The layers that decide whether a request to a subgraph or connector source goes ahead.
//!
//! Each admission layer is one unit: an outer buffer, the mapping that renders traffic-shaping
//! errors as GraphQL responses, a `load_shed`, and the rate limit it sheds for. They stay together
//! because each part depends on the one around it:
//!
//! - `load_shed` must observe the rate limiter's readiness without an intervening layer that
//!   absorbs backpressure.
//! - The outer buffer drives the admission stack and shares it across service clones. The inner
//!   buffer polls readiness unconstrained so cooperative scheduling is not mistaken for overload.
//! - The error mapping renders [`Overloaded`] from `load_shed` and [`Elapsed`] from a timeout
//!   placed anywhere beneath it, so the per-target timeout layer must be placed below this one.

use apollo_federation::connectors::runtime::errors::Error;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::limit::RateLimitLayer;
use tower::load_shed::error::Overloaded;
use tower::timeout::error::Elapsed;

use super::gateway_timeout_error;
use super::rate_limit_error;
use crate::layers::ServiceBuilderExt as _;
use crate::services::SubgraphResponse;
use crate::services::connector::request_service;
use crate::services::subgraph;

/// Layer type for [`TrafficShaping::subgraph_admission_layer`](super::TrafficShaping::subgraph_admission_layer).
pub(crate) struct SubgraphAdmissionLayer {
    rate_limit: Option<RateLimitLayer>,
}

impl SubgraphAdmissionLayer {
    pub(super) fn new(rate_limit: Option<RateLimitLayer>) -> Self {
        Self { rate_limit }
    }
}

impl<S> Layer<S> for SubgraphAdmissionLayer
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError> + Send + 'static,
    S::Future: Send + 'static,
{
    type Service = subgraph::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .buffered()
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
            .load_shed()
            .option_layer(self.rate_limit.clone())
            .service(inner)
            .boxed_clone()
    }
}

/// Layer type for [`TrafficShaping::connector_source_admission_layer`](super::TrafficShaping::connector_source_admission_layer).
pub(crate) struct ConnectorSourceAdmissionLayer {
    rate_limit: Option<RateLimitLayer>,
}

impl ConnectorSourceAdmissionLayer {
    pub(super) fn new(rate_limit: Option<RateLimitLayer>) -> Self {
        Self { rate_limit }
    }
}

impl<S> Layer<S> for ConnectorSourceAdmissionLayer
where
    S: Service<request_service::Request, Response = request_service::Response, Error = BoxError>
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = request_service::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .buffered()
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
            .load_shed()
            .option_layer(self.rate_limit.clone())
            .service(inner)
            .boxed_clone()
    }
}
