//! Utilities used for [`super::AxumHttpServerFactory`]

use std::net::SocketAddr;

use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use tower_http::trace::MakeSpan;
use tower_service::Service;
use tracing::Span;

use crate::plugins::telemetry::SpanMode;
use crate::plugins::telemetry::OTEL_STATUS_CODE;
use crate::plugins::telemetry::OTEL_STATUS_CODE_ERROR;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;

pub(crate) const REQUEST_SPAN_NAME: &str = "request";

#[derive(Clone, Default)]
pub(crate) struct PropagatingMakeSpan {
    pub(crate) license: LicenseState,
    pub(crate) span_mode: SpanMode,
}

impl<B> MakeSpan<B> for PropagatingMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        // This method needs to be moved to the telemetry plugin once we have a hook for the http request.

        // Before we make the span we need to attach span info that may have come in from the request.
        let context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&opentelemetry_http::HeaderExtractor(request.headers()))
        });
        let use_legacy_request_span = matches!(self.span_mode, SpanMode::Deprecated);

        // If there was no span from the request then it will default to the NOOP span.
        // Attaching the NOOP span has the effect of preventing further tracing.
        let span = if context.span().span_context().is_valid()
            || context.span().span_context().trace_id() != opentelemetry::trace::TraceId::INVALID
        {
            // We have a valid remote span, attach it to the current thread before creating the root span.
            let _context_guard = context.attach();
            if use_legacy_request_span {
                self.span_mode.create_request(request, self.license)
            } else {
                self.span_mode.create_router(request)
            }
        } else {
            // No remote span, we can go ahead and create the span without context.
            if use_legacy_request_span {
                self.span_mode.create_request(request, self.license)
            } else {
                self.span_mode.create_router(request)
            }
        };
        if matches!(
            self.license,
            LicenseState::LicensedWarn | LicenseState::LicensedHalt
        ) {
            span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            span.record("apollo_router.license", LICENSE_EXPIRED_SHORT_MESSAGE);
        }

        span
    }
}

pub(crate) struct InjectConnectionInfo<S> {
    inner: S,
    connection_info: ConnectionInfo,
}

#[derive(Clone)]
pub(crate) struct ConnectionInfo {
    pub(crate) peer_address: Option<SocketAddr>,
    pub(crate) server_address: Option<SocketAddr>,
}

impl<S> InjectConnectionInfo<S> {
    pub(crate) fn new(service: S, connection_info: ConnectionInfo) -> Self {
        InjectConnectionInfo {
            inner: service,
            connection_info,
        }
    }
}

impl<S, B> Service<http::Request<B>> for InjectConnectionInfo<S>
where
    S: Service<http::Request<B>>,
{
    type Response = <S as Service<http::Request<B>>>::Response;

    type Error = <S as Service<http::Request<B>>>::Error;

    type Future = <S as Service<http::Request<B>>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        req.extensions_mut().insert(self.connection_info.clone());
        self.inner.call(req)
    }
}
