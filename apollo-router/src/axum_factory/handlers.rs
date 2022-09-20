//! Http handlers
use std::str::FromStr;

use axum::body::StreamBody;
use axum::extract::Host;
use axum::extract::OriginalUri;
use axum::http::header::HeaderMap;
use axum::http::StatusCode;
use axum::response::*;
use bytes::Bytes;
use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use http::header::CONTENT_TYPE;
use http::HeaderValue;
use http::Request;
use http::Uri;
use hyper::Body;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceExt;
use tower_service::Service;

use super::utils::prefers_html;
use super::utils::process_vary_header;
use crate::graphql;
use crate::http_ext;
use crate::plugins::traffic_shaping::Elapsed;
use crate::plugins::traffic_shaping::RateLimited;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;

pub(super) async fn handle_get_with_static(
    static_page: Bytes,
    Host(host): Host,
    service: BoxService<
        http::Request<graphql::Request>,
        http::Response<graphql::ResponseStream>,
        BoxError,
    >,
    http_request: Request<Body>,
) -> impl IntoResponse {
    if prefers_html(http_request.headers()) {
        return Html(static_page).into_response();
    }

    if let Some(request) = http_request
        .uri()
        .query()
        .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
    {
        let mut http_request = http_request.map(|_| request);
        *http_request.uri_mut() = Uri::from_str(&format!("http://{}{}", host, http_request.uri()))
            .expect("the URL is already valid because it comes from axum; qed");
        return run_graphql_request(service, http_request)
            .await
            .into_response();
    }

    (StatusCode::BAD_REQUEST, "Invalid GraphQL request").into_response()
}

pub(super) async fn handle_get(
    Host(host): Host,
    service: BoxService<
        http::Request<graphql::Request>,
        http::Response<graphql::ResponseStream>,
        BoxError,
    >,
    http_request: Request<Body>,
) -> impl IntoResponse {
    if let Some(request) = http_request
        .uri()
        .query()
        .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
    {
        let mut http_request = http_request.map(|_| request);
        *http_request.uri_mut() = Uri::from_str(&format!("http://{}{}", host, http_request.uri()))
            .expect("the URL is already valid because it comes from axum; qed");
        return run_graphql_request(service, http_request)
            .await
            .into_response();
    }

    (StatusCode::BAD_REQUEST, "Invalid Graphql request").into_response()
}

pub(super) async fn handle_post(
    Host(host): Host,
    OriginalUri(uri): OriginalUri,
    Json(request): Json<graphql::Request>,
    service: BoxService<
        http::Request<graphql::Request>,
        http::Response<graphql::ResponseStream>,
        BoxError,
    >,
    header_map: HeaderMap,
) -> impl IntoResponse {
    let mut http_request = Request::post(
        Uri::from_str(&format!("http://{}{}", host, uri))
            .expect("the URL is already valid because it comes from axum; qed"),
    )
    .body(request)
    .expect("body has already been parsed; qed");
    *http_request.headers_mut() = header_map;

    run_graphql_request(service, http_request)
        .await
        .into_response()
}

async fn run_graphql_request<RS>(
    service: RS,
    http_request: Request<graphql::Request>,
) -> impl IntoResponse
where
    RS: Service<
            http::Request<graphql::Request>,
            Response = http::Response<graphql::ResponseStream>,
            Error = BoxError,
        > + Send,
{
    match service.ready_oneshot().await {
        Ok(mut service) => {
            let (head, body) = http_request.into_parts();

            match service.call(Request::from_parts(head, body)).await {
                Err(e) => {
                    if let Some(source_err) = e.source() {
                        if source_err.is::<RateLimited>() {
                            return RateLimited::new().into_response();
                        }
                        if source_err.is::<Elapsed>() {
                            return Elapsed::new().into_response();
                        }
                    }
                    tracing::error!("router service call failed: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "router service call failed",
                    )
                        .into_response()
                }
                Ok(response) => {
                    let (mut parts, mut stream) = response.into_parts();

                    process_vary_header(&mut parts.headers);

                    match stream.next().await {
                        None => {
                            tracing::error!("router service is not available to process request",);
                            (
                                StatusCode::SERVICE_UNAVAILABLE,
                                "router service is not available to process request",
                            )
                                .into_response()
                        }
                        Some(response) => {
                            if response.has_next.unwrap_or(false) {
                                parts.headers.insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
                                );

                                // each chunk contains a response and the next delimiter, to let client parsers
                                // know that they can process the response right away
                                let mut first_buf = Vec::from(
                                    &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"[..],
                                );
                                serde_json::to_writer(&mut first_buf, &response).unwrap();
                                first_buf.extend_from_slice(b"\r\n--graphql\r\n");

                                let body = once(ready(Ok(Bytes::from(first_buf)))).chain(
                                    stream.map(|res| {
                                        let mut buf = Vec::from(
                                            &b"content-type: application/json\r\n\r\n"[..],
                                        );
                                        serde_json::to_writer(&mut buf, &res).unwrap();

                                        // the last chunk has a different end delimiter
                                        if res.has_next.unwrap_or(false) {
                                            buf.extend_from_slice(b"\r\n--graphql\r\n");
                                        } else {
                                            buf.extend_from_slice(b"\r\n--graphql--\r\n");
                                        }

                                        Ok::<_, BoxError>(buf.into())
                                    }),
                                );

                                (parts, StreamBody::new(body)).into_response()
                            } else {
                                parts.headers.insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static("application/json"),
                                );
                                tracing::trace_span!("serialize_response").in_scope(|| {
                                    http_ext::Response::from(http::Response::from_parts(
                                        parts, response,
                                    ))
                                    .into_response()
                                })
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("router service is not available to process request: {}", e);
            if let Some(source_err) = e.source() {
                if source_err.is::<RateLimited>() {
                    return RateLimited::new().into_response();
                }
                if source_err.is::<Elapsed>() {
                    return Elapsed::new().into_response();
                }
            }

            (
                StatusCode::SERVICE_UNAVAILABLE,
                "router service is not available to process request",
            )
                .into_response()
        }
    }
}
