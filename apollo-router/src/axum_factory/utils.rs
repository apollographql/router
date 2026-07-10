//! Utilities used for [`super::AxumHttpServerFactory`]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_http::trace::MakeSpan;
use tracing::Span;

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
/// `router::BoxCloneService` is `Send` but not `Sync` (tower's `BoxCloneService` only requires
/// `+ Send` on its inner trait object), while axum's `Extension`/`http::Extensions` require
/// `Send + Sync`. `Mutex<T>` is `Sync` for any `T: Send`, so wrapping it here is what makes it
/// storable as a per-connection `Extension`; each request briefly locks it just to clone the
/// inner service out before calling it, never holding the lock across an `.await`.
pub(crate) type ConnectionRouterService = Arc<Mutex<router::BoxCloneService>>;

/// Wraps a freshly created router service (`RouterFactory::create()`) for use across a single
/// connection.
///
/// Adds a bare `tower::load_shed()` with no preceding `Buffer`: if the pipeline's `poll_ready`
/// stays `Pending`, new requests on this connection are shed immediately (`Overloaded`) instead
/// of piling up until the transport's concurrency limit (e.g. HTTP/2 max concurrent streams) is
/// hit. This differs from `plugins/traffic_shaping/mod.rs`'s `buffered().load_shed()` pairing,
/// which exists because layers whose `poll_ready` polls an `mpsc`-backed resource (`Buffer`,
/// `RateLimit`) can otherwise return a spurious `Pending` due to Tokio's cooperative-scheduling
/// budget, producing a false `Overloaded`. By default every `poll_ready` in the
/// router/supergraph/execution/subgraph chain is a trivial `Poll::Ready(Ok(()))` passthrough with
/// no such resource to poll, so that failure mode doesn't apply at this outermost boundary; when
/// `traffic_shaping` is configured with real backpressure, it already guards its own internals.
pub(crate) fn connection_router_service(
    service: router::BoxCloneService,
) -> ConnectionRouterService {
    Arc::new(Mutex::new(
        ServiceBuilder::new()
            .load_shed()
            .service(service)
            .boxed_clone(),
    ))
}

#[cfg(test)]
mod tests {
    use std::task::Context;
    use std::task::Poll;

    use tower::BoxError;
    use tower::Service;

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
        let connection_service = connection_router_service(NeverReady.boxed_clone());
        let service = connection_service
            .lock()
            .expect("router service mutex poisoned")
            .clone();

        let request = router::Request::fake_builder()
            .build()
            .expect("fake request should build");

        let err = service
            .oneshot(request)
            .await
            .expect_err("a permanently-pending service should be shed, not succeed");

        assert!(
            err.is::<tower::load_shed::error::Overloaded>(),
            "expected an Overloaded error, got: {err}"
        );
    }
}
