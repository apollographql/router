//! Utilities used for [`super::AxumHttpServerFactory`]

use std::net::SocketAddr;
use std::sync::Arc;

use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use tower::ServiceBuilder;
use tower_http::trace::MakeSpan;
use tracing::Span;

use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
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

/// The router pipeline created for a single connection (or, in tests, a single in-process
/// session), shared across all requests on that connection/session.
///
/// Stored as an axum `Extension`, which requires `Send + Sync`. The `Send`-only
/// `router::BoxCloneService` returned by `RouterFactory::create()` gains `Sync` when wrapped in
/// `UnconstrainedBuffer` inside [`connection_router_service`], because `Buffer` is backed by
/// channel handles that are `Send + Sync` regardless of the wrapped service's `Sync`-ness.
pub(crate) type ConnectionRouterService =
    tower::util::BoxCloneSyncService<router::Request, router::Response, tower::BoxError>;

/// Wraps a freshly created router service for use across a single connection.
///
/// Builds the stack `UnconstrainedBuffer(load_shed(service))`:
///
/// - The `load_shed` directly observes the service's `poll_ready`: if the pipeline signals
///   `Poll::Pending`, new requests on this connection are immediately shed with `Overloaded`
///   (→ 503) rather than piling up indefinitely.
/// - The `UnconstrainedBuffer` sits outside the `load_shed` so that:
///   1. Requests are queued and dispatched to the `load_shed` worker without holding up the
///      caller's `poll_ready` path.
///   2. Polling is unconstrained, preventing Tokio's cooperative-scheduling budget from
///      producing spurious `Poll::Pending` returns that would cause false `Overloaded` errors
///      (see [`crate::layers::unconstrained_buffer`]).
///   3. `Buffer` is backed by channel handles that are `Send + Sync` regardless of the wrapped
///      service's `Sync`-ness, converting the `Send`-only `router::BoxCloneService` into a type
///      suitable for storage as an axum `Extension`.
pub(crate) fn connection_router_service(
    service: router::BoxCloneService,
) -> ConnectionRouterService {
    ConnectionRouterService::new(UnconstrainedBuffer::new(
        ServiceBuilder::new().load_shed().service(service),
        DEFAULT_BUFFER_SIZE,
    ))
}

#[cfg(test)]
mod tests {
    use std::task::Context;
    use std::task::Poll;

    use tower::BoxError;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;

    /// A service that never becomes ready, so `call` should never be invoked.
    #[derive(Clone)]
    struct NeverReady;

    impl Service<router::Request> for NeverReady {
        type Response = router::Response;
        type Error = BoxError;
        type Future = std::future::Pending<Result<router::Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn call(&mut self, _req: router::Request) -> Self::Future {
            unreachable!("load_shed should short-circuit calls to a never-ready service")
        }
    }

    #[tokio::test]
    async fn connection_router_service_sheds_load_instead_of_blocking() {
        let connection_service =
            connection_router_service(router::BoxCloneService::new(NeverReady));

        let request = router::Request::fake_builder()
            .build()
            .expect("fake request should build");

        let err = connection_service
            .oneshot(request)
            .await
            .expect_err("a permanently-pending service should be shed, not succeed");

        assert!(
            err.is::<tower::load_shed::error::Overloaded>(),
            "expected an Overloaded error, got: {err}"
        );
    }
}
