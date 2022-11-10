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

use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::services::MULTIPART_DEFER_SPEC_PARAMETER;
use crate::services::MULTIPART_DEFER_SPEC_VALUE;

pub(crate) const REQUEST_SPAN_NAME: &str = "request";
pub(crate) const APPLICATION_JSON_HEADER_VALUE: &str = "application/json";
pub(crate) const GRAPHQL_JSON_RESPONSE_HEADER_VALUE: &str = "application/graphql-response+json";

pub(super) fn prefers_html(headers: &HeaderMap) -> bool {
    let text_html = MediaType::new(TEXT, HTML);

    headers.get_all(&http::header::ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| mime.as_ref() == Ok(&text_html))
            })
            .unwrap_or(false)
    })
}

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

pub(super) async fn check_accept_header(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, Response> {
    let ask_for_html = req.method() == Method::GET && prefers_html(req.headers());

    if accepts_wildcard(req.headers())
        || ask_for_html
        || accepts_multipart(req.headers())
        || accepts_json(req.headers())
    {
        Ok(next.run(req).await)
    } else {
        Err((
            StatusCode::NOT_ACCEPTABLE,
            format!(
                r#"'accept' header can't be different than \"*/*\", {:?}, {:?} or {:?}"#,
                APPLICATION_JSON_HEADER_VALUE,
                GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                MULTIPART_DEFER_CONTENT_TYPE
            ),
        )
            .into_response())
    }
}

/// Returns true if the headers contain header `accept: */*`
pub(crate) fn accepts_wildcard(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| accept_str == "*/*")
            .unwrap_or(false)
    })
}

/// Returns true if the headers contain `accept: application/json` or `accept: application/graphql-response+json`,
/// or if there is no `accept` header
pub(crate) fn accepts_json(headers: &HeaderMap) -> bool {
    !headers.contains_key(ACCEPT)
        || headers.get_all(ACCEPT).iter().any(|value| {
            value
                .to_str()
                .map(|accept_str| {
                    let mut list = MediaTypeList::new(accept_str);

                    list.any(|mime| {
                        mime.as_ref()
                            .map(|mime| {
                                (mime.ty == APPLICATION && mime.subty == JSON)
                                    || (mime.ty == APPLICATION
                                        && mime.subty.as_str() == "graphql-response"
                                        && mime.suffix == Some(JSON))
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
}

/// Returns true if the headers contain accept header to enable defer
pub(crate) fn accepts_multipart(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| {
                    mime.as_ref()
                        .map(|mime| {
                            mime.ty == MULTIPART
                                && mime.subty == MIXED
                                && mime.get_param(
                                    mediatype::Name::new(MULTIPART_DEFER_SPEC_PARAMETER)
                                        .expect("valid name"),
                                ) == Some(
                                    mediatype::Value::new(MULTIPART_DEFER_SPEC_VALUE)
                                        .expect("valid value"),
                                )
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
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
        if context.span().span_context().is_valid() {
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

    #[test]
    fn it_checks_accept_header() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        assert!(accepts_json(&default_headers));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        assert!(accepts_wildcard(&default_headers));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(ACCEPT, HeaderValue::from_static("foo/bar"));
        assert!(accepts_json(&default_headers));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
        );
        default_headers.append(
            ACCEPT,
            HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
        );
        assert!(accepts_multipart(&default_headers));
    }
}
