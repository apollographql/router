//! Http handlers

// use axum::http::StatusCode;
// use axum::response::*;
// use bytes::Bytes;
// use http::Request;
// use hyper::Body;
// use tower::util::BoxService;
// use tower::BoxError;
// use tower::ServiceExt;
// use tower_service::Service;

// use super::utils::prefers_html;
// use crate::plugins::traffic_shaping::Elapsed;
// use crate::plugins::traffic_shaping::RateLimited;
// use crate::services::router;
// use crate::RouterRequest;
// use crate::RouterResponse;

// pub(super) async fn handle_get_with_static(
//     static_page: Bytes,
//     service: router::BoxService,
//     http_request: Request<Body>,
// ) -> impl IntoResponse {
//     if prefers_html(http_request.headers()) {
//         return Html(static_page).into_response();
//     }
//     run_graphql_request(service, http_request)
//         .await
//         .into_response()
// }

// pub(super) async fn handle_get(
//     service: router::BoxService,
//     http_request: Request<Body>,
// ) -> impl IntoResponse {
//     run_graphql_request(service, http_request)
//         .await
//         .into_response()
// }

// pub(super) async fn handle_post(
//     http_request: Request<Body>,
//     service: BoxService<RouterRequest, RouterResponse, BoxError>,
// ) -> impl IntoResponse {
//     run_graphql_request(service, http_request)
//         .await
//         .into_response()
// }

// async fn run_graphql_request<RS>(service: RS, http_request: Request<Body>) -> impl IntoResponse
// where
//     RS: Service<RouterRequest, Response = RouterResponse, Error = BoxError> + Send,
// {

//     // let (head, body) = http_request.into_parts();
//     // let mut req: SupergraphRequest = Request::from_parts(head, body).into();
//     // req = match apq.apq_request(req).await {
//     //     Ok(req) => req,
//     //     Err(res) => {
//     //         let (parts, mut stream) = res.response.into_parts();

//     //         return match stream.next().await {
//     //             None => {
//     //                 tracing::error!("router service is not available to process request",);
//     //                 (
//     //                     StatusCode::SERVICE_UNAVAILABLE,
//     //                     "router service is not available to process request",
//     //                 )
//     //                     .into_response()
//     //             }
//     //             Some(body) => http_ext::Response::from(http::Response::from_parts(parts, body))
//     //                 .into_response(),
//     //         };
//     //     }
//     // };

//     // match service.ready_oneshot().await {
//     //     Ok(mut service) => {
//     //         let accepts_multipart = accepts_multipart(req.supergraph_request.headers());
//     //         let accepts_json = accepts_json(req.supergraph_request.headers());
//     //         let accepts_wildcard = accepts_wildcard(req.supergraph_request.headers());

//     //         match service.call(req).await {
//     //             Err(e) => {
//     //                 if let Some(source_err) = e.source() {
//     //                     if source_err.is::<RateLimited>() {
//     //                         return RateLimited::new().into_response();
//     //                     }
//     //                     if source_err.is::<Elapsed>() {
//     //                         return Elapsed::new().into_response();
//     //                     }
//     //                 }
//     //                 tracing::error!("router service call failed: {}", e);
//     //                 (
//     //                     StatusCode::INTERNAL_SERVER_ERROR,
//     //                     "router service call failed",
//     //                 )
//     //                     .into_response()
//     //             }
//     //             Ok(response) => {
//     //                 let (mut parts, mut stream) = response.response.into_parts();

//     //                 process_vary_header(&mut parts.headers);

//     //                 match stream.next().await {
//     //                     None => {
//     //                         tracing::error!("router service is not available to process request",);
//     //                         (
//     //                             StatusCode::SERVICE_UNAVAILABLE,
//     //                             "router service is not available to process request",
//     //                         )
//     //                             .into_response()
//     //                     }
//     //                     Some(response) => {
//     //                         if !response.has_next.unwrap_or(false)
//     //                             && (accepts_json || accepts_wildcard)
//     //                         {
//     //                             parts.headers.insert(
//     //                                 CONTENT_TYPE,
//     //                                 HeaderValue::from_static("application/json"),
//     //                             );
//     //                             tracing::trace_span!("serialize_response").in_scope(|| {
//     //                                 http_ext::Response::from(http::Response::from_parts(
//     //                                     parts, response,
//     //                                 ))
//     //                                 .into_response()
//     //                             })
//     //                         } else if accepts_multipart {
//     //                             parts.headers.insert(
//     //                                 CONTENT_TYPE,
//     //                                 HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
//     //                             );

//     //                             // each chunk contains a response and the next delimiter, to let client parsers
//     //                             // know that they can process the response right away
//     //                             let mut first_buf = Vec::from(
//     //                                 &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"[..],
//     //                             );
//     //                             serde_json::to_writer(&mut first_buf, &response).unwrap();
//     //                             if response.has_next.unwrap_or(false) {
//     //                                 first_buf.extend_from_slice(b"\r\n--graphql\r\n");
//     //                             } else {
//     //                                 first_buf.extend_from_slice(b"\r\n--graphql--\r\n");
//     //                             }

//     //                             let body = once(ready(Ok(Bytes::from(first_buf)))).chain(
//     //                                 stream.map(|res| {
//     //                                     let mut buf = Vec::from(
//     //                                         &b"content-type: application/json\r\n\r\n"[..],
//     //                                     );
//     //                                     serde_json::to_writer(&mut buf, &res).unwrap();

//     //                                     // the last chunk has a different end delimiter
//     //                                     if res.has_next.unwrap_or(false) {
//     //                                         buf.extend_from_slice(b"\r\n--graphql\r\n");
//     //                                     } else {
//     //                                         buf.extend_from_slice(b"\r\n--graphql--\r\n");
//     //                                     }

//     //                                     Ok::<_, BoxError>(buf.into())
//     //                                 }),
//     //                             );

//     //                             (parts, StreamBody::new(body)).into_response()
//     //                         } else {
//     //                             // this should be unreachable due to a previous check, but just to be sure...
//     //                             (
//     //                                 StatusCode::NOT_ACCEPTABLE,
//     //                                 format!(
//     //                                     r#"'accept' header can't be different than \"*/*\", {:?}, {:?} or {:?}"#,
//     //                                     APPLICATION_JSON_HEADER_VALUE,
//     //                                     GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
//     //                                     MULTIPART_DEFER_CONTENT_TYPE
//     //                                 ),
//     //                             )
//     //                                 .into_response()
//     //                         }
//     //                     }
//     //                 }
//     //             }
//     //         }
//     //     }
// }
