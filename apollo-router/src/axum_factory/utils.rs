//! Utilities used for [`super::AxumHttpServerFactory`]

use async_compression::tokio::write::BrotliDecoder;
use async_compression::tokio::write::GzipDecoder;
use async_compression::tokio::write::ZlibDecoder;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::*;
use futures::prelude::*;
use http::header::CONTENT_ENCODING;
use http::Request;
use hyper::Body;
use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use tokio::io::AsyncWriteExt;
use tower_http::trace::MakeSpan;
use tracing::Span;

use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;

pub(crate) const REQUEST_SPAN_NAME: &str = "request";

pub(super) async fn decompress_request_body(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, Response> {
    let (parts, body) = req.into_parts();
    let content_encoding = parts.headers.get(&CONTENT_ENCODING);
    macro_rules! decode_body {
        ($decoder: ident, $error_message: expr) => {{
            let body_bytes = hyper::body::to_bytes(body)
                .map_err(|err| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("cannot read request body: {err}"),
                    )
                        .into_response()
                })
                .await?;
            let mut decoder = $decoder::new(Vec::new());
            decoder.write_all(&body_bytes).await.map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("{}: {err}", $error_message),
                )
                    .into_response()
            })?;
            decoder.shutdown().await.map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("{}: {err}", $error_message),
                )
                    .into_response()
            })?;

            Ok(next
                .run(Request::from_parts(parts, Body::from(decoder.into_inner())))
                .await)
        }};
    }

    match content_encoding {
        Some(content_encoding) => match content_encoding.to_str() {
            Ok(content_encoding_str) => match content_encoding_str {
                "br" => decode_body!(BrotliDecoder, "cannot decompress (brotli) request body"),
                "gzip" => decode_body!(GzipDecoder, "cannot decompress (gzip) request body"),
                "deflate" => decode_body!(ZlibDecoder, "cannot decompress (deflate) request body"),
                "identity" => Ok(next.run(Request::from_parts(parts, body)).await),
                unknown => {
                    let message = format!("unknown content-encoding header value {unknown:?}");
                    tracing::error!(message);
                    ::tracing::error!(
                       monotonic_counter.apollo_router_http_requests_total = 1u64,
                       status = %400u16,
                       error = %message,
                    );

                    Err((StatusCode::BAD_REQUEST, message).into_response())
                }
            },

            Err(err) => {
                let message = format!("cannot read content-encoding header: {err}");
                ::tracing::error!(
                   monotonic_counter.apollo_router_http_requests_total = 1u64,
                   status = %400u16,
                   error = %message,
                );
                Err((StatusCode::BAD_REQUEST, message).into_response())
            }
        },
        None => Ok(next.run(Request::from_parts(parts, body)).await),
    }
}

#[derive(Clone, Default)]
pub(crate) struct PropagatingMakeSpan {
    pub(crate) license: LicenseState,
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
        if context.span().span_context().is_valid()
            || context.span().span_context().trace_id() != opentelemetry::trace::TraceId::INVALID
        {
            // We have a valid remote span, attach it to the current thread before creating the root span.
            let _context_guard = context.attach();
            self.create_span(request)
        } else {
            // No remote span, we can go ahead and create the span without context.
            self.create_span(request)
        }
    }
}

impl PropagatingMakeSpan {
    fn create_span<B>(&mut self, request: &Request<B>) -> Span {
        if matches!(
            self.license,
            LicenseState::LicensedWarn | LicenseState::LicensedHalt
        ) {
            tracing::error_span!(
                REQUEST_SPAN_NAME,
                "http.method" = %request.method(),
                "http.route" = %request.uri(),
                "http.flavor" = ?request.version(),
                "http.status" = 500, // This prevents setting later
                "otel.name" = ::tracing::field::Empty,
                "otel.kind" = "SERVER",
                "graphql.operation.name" = ::tracing::field::Empty,
                "graphql.operation.type" = ::tracing::field::Empty,
                "apollo_router.license" = LICENSE_EXPIRED_SHORT_MESSAGE,
                "apollo_private.request" = true,
            )
        } else {
            tracing::info_span!(
                REQUEST_SPAN_NAME,
                "http.method" = %request.method(),
                "http.route" = %request.uri(),
                "http.flavor" = ?request.version(),
                "otel.name" = ::tracing::field::Empty,
                "otel.kind" = "SERVER",
                "graphql.operation.name" = ::tracing::field::Empty,
                "graphql.operation.type" = ::tracing::field::Empty,
                "apollo_private.request" = true,
            )
        }
    }
}
