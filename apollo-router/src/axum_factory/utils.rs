//! Utilities used for [`super::AxumHttpServerFactory`]

use async_compression::tokio::write::BrotliDecoder;
use async_compression::tokio::write::GzipDecoder;
use async_compression::tokio::write::ZlibDecoder;
use axum::http::header::HeaderMap;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::*;
use futures::prelude::*;
use http::header::ACCEPT;
use http::header::CONTENT_ENCODING;
use http::header::VARY;
use http::HeaderValue;
use http::Method;
use http::Request;
use hyper::Body;
use mediatype::names::APPLICATION;
use mediatype::names::HTML;
use mediatype::names::JSON;
use mediatype::names::MIXED;
use mediatype::names::MULTIPART;
use mediatype::names::TEXT;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use opentelemetry::global;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::TraceContextExt;
use tokio::io::AsyncWriteExt;
use tower_http::trace::MakeSpan;
use tracing::Level;
use tracing::Span;

use crate::services::MULTIPART_DEFER_SPEC_PARAMETER;
use crate::services::MULTIPART_DEFER_SPEC_VALUE;

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
                    tracing::error!("unknown content-encoding header value {:?}", unknown);
                    Err((
                        StatusCode::BAD_REQUEST,
                        format!("unknown content-encoding header value: {unknown:?}"),
                    )
                        .into_response())
                }
            },

            Err(err) => Err((
                StatusCode::BAD_REQUEST,
                format!("cannot read content-encoding header: {err}"),
            )
                .into_response()),
        },
        None => Ok(next.run(Request::from_parts(parts, body)).await),
    }
}

// Process the headers to make sure that `VARY` is set correctly
pub(super) fn process_vary_header(headers: &mut HeaderMap<HeaderValue>) {
    if headers.get(VARY).is_none() {
        // We don't have a VARY header, add one with value "origin"
        headers.insert(VARY, HeaderValue::from_static("origin"));
    }
}

#[derive(Clone)]
pub(super) struct PropagatingMakeSpan;

impl PropagatingMakeSpan {
    pub(super) fn new() -> Self {
        Self {}
    }
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
            tracing::span!(
                Level::INFO,
                REQUEST_SPAN_NAME,
                "http.method" = %request.method(),
                "http.route" = %request.uri(),
                "http.flavor" = ?request.version(),
                "otel.kind" = %SpanKind::Server,
                "otel.status_code" = tracing::field::Empty,
                "apollo_private.duration_ns" = tracing::field::Empty,
                "trace_id" = tracing::field::Empty
            )
        } else {
            // No remote span, we can go ahead and create the span without context.
            tracing::span!(
                Level::INFO,
                REQUEST_SPAN_NAME,
                "http.method" = %request.method(),
                "http.route" = %request.uri(),
                "http.flavor" = ?request.version(),
                "otel.kind" = %SpanKind::Server,
                "otel.status_code" = tracing::field::Empty,
                "apollo_private.duration_ns" = tracing::field::Empty,
                "trace_id" = tracing::field::Empty
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test Vary processing

    #[test]
    fn it_adds_default_with_value_origin_if_no_vary_header() {
        let mut default_headers = HeaderMap::new();
        process_vary_header(&mut default_headers);
        let vary_opt = default_headers.get(VARY);
        assert!(vary_opt.is_some());
        let vary = vary_opt.expect("has a value");
        assert_eq!(vary, "origin");
    }

    #[test]
    fn it_leaves_vary_alone_if_set() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(VARY, HeaderValue::from_static("*"));
        process_vary_header(&mut default_headers);
        let vary_opt = default_headers.get(VARY);
        assert!(vary_opt.is_some());
        let vary = vary_opt.expect("has a value");
        assert_eq!(vary, "*");
    }

    #[test]
    fn it_leaves_varys_alone_if_there_are_more_than_one() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(VARY, HeaderValue::from_static("one"));
        default_headers.append(VARY, HeaderValue::from_static("two"));
        process_vary_header(&mut default_headers);
        let vary = default_headers.get_all(VARY);
        assert_eq!(vary.iter().count(), 2);
        for value in vary {
            assert!(value == "one" || value == "two");
        }
    }
}
