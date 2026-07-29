//! Utilities used for [`super::AxumHttpServerFactory`]

use std::net::SocketAddr;
use std::sync::Arc;

use http::StatusCode;
use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use tower::ServiceBuilder;
use tower::load_shed::error::Overloaded;
use tower_http::trace::MakeSpan;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::span_factory;
use crate::services::router;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;
use crate::uplink::license_enforcement::LicenseState;

#[derive(Clone, Default)]
pub(crate) struct PropagatingMakeSpan {
    pub(crate) license: Arc<LicenseState>,
}

impl<B> MakeSpan<B> for PropagatingMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        // This method needs to be moved to the telemetry plugin once we have a hook for the http request.

        // Before we make the span we need to attach span info that may have come in from the request.
        let context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&opentelemetry_http::HeaderExtractor(request.headers()))
        });

        // If there was no span from the request then it will default to the NOOP span.
        // Attaching the NOOP span has the effect of preventing further tracing.
        let span = if context.span().span_context().is_valid()
            || context.span().span_context().trace_id() != opentelemetry::trace::TraceId::INVALID
        {
            // We have a valid remote span, attach it to the current thread before creating the root span.
            let _context_guard = context.attach();
            span_factory::create_router(request)
        } else {
            // No remote span, we can go ahead and create the span without context.
            span_factory::create_router(request)
        };
        if matches!(
            &*self.license,
            LicenseState::LicensedWarn { limits: _ } | LicenseState::LicensedHalt { limits: _ }
        ) {
            span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            span.record("apollo_router.license", LICENSE_EXPIRED_SHORT_MESSAGE);
        }

        span
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionInfo {
    pub(crate) peer_address: Option<SocketAddr>,
    pub(crate) server_address: Option<SocketAddr>,
}

pub(crate) type ConnectionRouterService =
    tower::util::BoxCloneSyncService<router::Request, router::Response, tower::BoxError>;

/// Wraps a router service so every connection can share one instance, cloning it per request.
///
/// Requests queue while the pipeline is busy and answer with a 503 once the queue is full,
/// rather than waiting without bound.
pub(crate) fn connection_router_service(
    service: router::BoxCloneService,
) -> ConnectionRouterService {
    ConnectionRouterService::new(
        ServiceBuilder::new()
            .map_future_with_request_data(|req: &router::Request| req.context.clone(), shed_as_503)
            // hyper awaits `poll_ready` inside each request's future, so a layer reporting
            // `Pending` there stalls that request instead of applying backpressure to the
            // connection. `load_shed` always reports `Ready`, so it has to sit above the queue
            // for the shed decision to be immediate.
            .load_shed()
            // Unconstrained, so tokio's cooperative budget cannot report a spurious `Pending`
            // and shed a request the queue had room for.
            .buffered()
            .service(service),
    )
}

/// Answers a shed request with a 503 so callers never handle [`Overloaded`] themselves.
///
/// Belongs directly outside the `load_shed` whose errors it renders.
async fn shed_as_503(
    context: Context,
    future: impl Future<Output = Result<router::Response, tower::BoxError>>,
) -> Result<router::Response, tower::BoxError> {
    match future.await {
        Err(err) if err.is::<Overloaded>() => {
            // Debug rather than warn: shedding is per-request, so a sustained overload would
            // otherwise emit a line per rejected request while the router is already struggling.
            // The 503 is counted by the `apollo.router.operations` metric.
            tracing::debug!(
                code = "REQUEST_CONCURRENCY_LIMITED",
                "the connection queue is full, shedding request",
            );

            Ok(router::Response::error_builder()
                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                .error(
                    graphql::Error::builder()
                        .message("Your request has been concurrency limited waiting for the router")
                        .extension_code("REQUEST_CONCURRENCY_LIMITED")
                        .build(),
                )
                .context(context)
                .build()
                .expect("overloaded response should build"))
        }
        other => other,
    }
}

/// Stands in for a saturated pipeline: `poll_ready` never resolves, so `load_shed` should
/// short-circuit before `call` is reached.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct NeverReady;

#[cfg(test)]
impl tower::Service<router::Request> for NeverReady {
    type Response = router::Response;
    type Error = tower::BoxError;
    type Future = std::future::Pending<Result<router::Response, tower::BoxError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Pending
    }

    fn call(&mut self, _req: router::Request) -> Self::Future {
        unreachable!("load_shed should short-circuit calls to a never-ready service")
    }
}

#[cfg(test)]
mod tests {
    use tower::Service;

    use super::*;

    /// An unready pipeline must not stall readiness, because hyper awaits `poll_ready` inside
    /// each request's future. Shedding once the queue fills is covered by
    /// `unconstrained_buffer::tests::full_buffer_should_still_cause_load_shedding`, which
    /// exercises the same `LoadShed` over `UnconstrainedBuffer` pairing with a capacity it can
    /// actually exhaust.
    // `Buffer` spawns its worker, so this needs a runtime even though nothing is awaited.
    #[tokio::test]
    async fn connection_router_service_always_reports_ready() {
        let mut connection_service =
            connection_router_service(router::BoxCloneService::new(NeverReady));

        let ready = futures::future::poll_fn(|cx| {
            std::task::Poll::Ready(connection_service.poll_ready(cx))
        })
        .await;

        assert!(
            matches!(ready, std::task::Poll::Ready(Ok(()))),
            "poll_ready must never report Pending to hyper"
        );
    }

    /// A shed request has to come back as a response rather than an error, so callers can return
    /// it without knowing that `load_shed` exists.
    #[tokio::test]
    async fn shed_requests_answer_with_a_503_graphql_error() {
        let response = shed_as_503(
            Context::new(),
            std::future::ready(Err(Overloaded::new().into())),
        )
        .await
        .expect("a shed request should answer, not error");

        assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = router::body::into_bytes(response.response.into_body())
            .await
            .expect("the body should be readable");
        let body: graphql::Response =
            serde_json::from_slice(&body).expect("the body should be a GraphQL response");

        let error = body.errors.first().expect("a GraphQL error is expected");
        assert_eq!(
            error.extensions.get("code").and_then(|code| code.as_str()),
            Some("REQUEST_CONCURRENCY_LIMITED")
        );
        assert_eq!(
            error.message,
            "Your request has been concurrency limited waiting for the router"
        );
    }

    /// Anything other than an `Overloaded` has to pass through untouched.
    #[tokio::test]
    async fn other_errors_pass_through() {
        let err = shed_as_503(
            Context::new(),
            std::future::ready(Err("something else".into())),
        )
        .await
        .expect_err("a non-overload error should stay an error");

        assert_eq!(err.to_string(), "something else");
    }
}
