//! Implements the router phase of the request lifecycle.

use std::task::Poll;

use axum::body::StreamBody;
use axum::response::*;
use bytes::Buf;
use bytes::Bytes;
use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream;
use futures::stream::once;
use futures::stream::StreamExt;
use http::header::CONTENT_TYPE;
use http::header::VARY;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::Request;
use http::StatusCode;
use http_body::Body as _;
use hyper::Body;
use multimap::MultiMap;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use super::new_service::ServiceFactory;
use super::router;
use super::supergraph;
use super::StuffThatHasPlugins;
use super::SupergraphCreator;
use super::MULTIPART_DEFER_CONTENT_TYPE;
use crate::graphql;
use crate::http_ext;
use crate::router_factory::RouterFactory;
use crate::Endpoint;
use crate::ListenAddr;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    supergraph_creator: SF,
}

impl<SF> RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(supergraph_creator: SF) -> Self {
        RouterService { supergraph_creator }
    }
}

impl<SF> Service<RouterRequest> for RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        let RouterRequest {
            router_request,
            context,
        } = req;

        let (parts, body) = router_request.into_parts();

        let supergraph_service = self.supergraph_creator.create();
        // TODO[igni]: deal with errors
        //     (StatusCode::BAD_REQUEST, "Invalid GraphQL request").into_response() will help
        let fut = async move {
            let graphql_request = if parts.method == Method::GET {
                parts
                    .uri
                    .query()
                    .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
                    .unwrap()
            } else {
                let bytes = hyper::body::to_bytes(body).await.unwrap();
                serde_json::from_reader(bytes.reader()).unwrap()
            };

            let request = SupergraphRequest {
                supergraph_request: http::Request::from_parts(parts, graphql_request),
                context,
            };

            let SupergraphResponse { response, context } =
                supergraph_service.oneshot(request).await.unwrap();

            let (mut parts, mut body) = response.into_parts();
            process_vary_header(&mut parts.headers);

            let response: Response<hyper::Body> = match body.next().await {
                None => {
                    tracing::error!("router service is not available to process request",);
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "router service is not available to process request",
                    )
                        .into_response()
                }
                Some(response) => {
                    let accepts_wildcard: bool = context
                        .get("accepts-wildcard")
                        .unwrap_or_default()
                        .unwrap_or_default();
                    let accepts_json: bool = context
                        .get("accepts-json")
                        .unwrap_or_default()
                        .unwrap_or_default();
                    let accepts_multipart: bool = context
                        .get("accepts-multipart")
                        .unwrap_or_default()
                        .unwrap_or_default();

                    if !response.has_next.unwrap_or(false) && (accepts_json || accepts_wildcard) {
                        parts
                            .headers
                            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                        tracing::trace_span!("serialize_response").in_scope(|| {
                            http_ext::Response::from(http::Response::from_parts(parts, response))
                                .into_response()
                        })
                    } else if accepts_multipart {
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
                        if response.has_next.unwrap_or(false) {
                            first_buf.extend_from_slice(b"\r\n--graphql\r\n");
                        } else {
                            first_buf.extend_from_slice(b"\r\n--graphql--\r\n");
                        }

                        let body = once(ready(Ok(Bytes::from(first_buf)))).chain(body.map(|res| {
                            let mut buf = Vec::from(&b"content-type: application/json\r\n\r\n"[..]);
                            serde_json::to_writer(&mut buf, &res).unwrap();

                            // the last chunk has a different end delimiter
                            if res.has_next.unwrap_or(false) {
                                buf.extend_from_slice(b"\r\n--graphql\r\n");
                            } else {
                                buf.extend_from_slice(b"\r\n--graphql--\r\n");
                            }

                            Ok::<_, BoxError>(buf.into())
                        }));

                        (parts, StreamBody::new(body)).into_response()
                    } else {
                        // this should be unreachable due to a previous check, but just to be sure...
                        unreachable!()
                    }
                }
            }
            .map(|body| {
                // Axum makes this `body` have type:
                // https://docs.rs/http-body/0.4.5/http_body/combinators/struct.UnsyncBoxBody.html
                let mut body = Box::pin(body);
                // We make a stream based on its `poll_data` method
                // in order to create a `hyper::Body`.
                Body::wrap_stream(stream::poll_fn(move |ctx| body.as_mut().poll_data(ctx)))
                // … but we ignore the `poll_trailers` method:
                // https://docs.rs/http-body/0.4.5/http_body/trait.Body.html#tymethod.poll_trailers
                // Apparently HTTP/2 trailers are like headers, except after the response body.
                // I (Simon) believe nothing in the Apollo Router uses trailers as of this writing,
                // so ignoring `poll_trailers` is fine.
                // If we want to use trailers, we may need remove this convertion to `hyper::Body`
                // and return `UnsyncBoxBody` (a.k.a. `axum::BoxBody`) as-is.
            })
            .into();

            Ok(RouterResponse { response, context })
        };
        Box::pin(fut)
    }
}

// Process the headers to make sure that `VARY` is set correctly
fn process_vary_header(headers: &mut HeaderMap<HeaderValue>) {
    if headers.get(VARY).is_none() {
        // We don't have a VARY header, add one with value "origin"
        headers.insert(VARY, HeaderValue::from_static("origin"));
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct RouterCreator<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    supergraph_creator: SF,
}

impl<SF> ServiceFactory<router::Request> for RouterCreator<SF>
where
    SF: StuffThatHasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    type Service = router::BoxService;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl<SF> RouterFactory for RouterCreator<SF>
where
    SF: StuffThatHasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    type RouterService = router::BoxService;

    type Future = <<RouterCreator<SF> as ServiceFactory<router::Request>>::Service as Service<
        router::Request,
    >>::Future;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut mm = MultiMap::new();
        self.supergraph_creator
            .plugins()
            .values()
            .for_each(|p| mm.extend(p.web_endpoints()));
        mm
    }
}

impl<SF> RouterCreator<SF>
where
    SF: StuffThatHasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    pub(crate) fn new(supergraph_creator: SF) -> Self {
        Self { supergraph_creator }
    }
    pub(crate) fn make(
        &self,
    ) -> impl Service<
        router::Request,
        Response = router::Response,
        Error = BoxError,
        Future = BoxFuture<'static, router::ServiceResult>,
    > + Send {
        let router_service = RouterService::new(self.supergraph_creator.clone());

        ServiceBuilder::new().service(
            self.supergraph_creator
                .plugins()
                .iter()
                .rev()
                .fold(router_service.boxed(), |acc, (_, e)| e.router_service(acc)),
        )
    }

    /// Create a test service.
    #[cfg(test)]
    pub(crate) fn test_service(&self) -> router::BoxCloneService {
        use tower::buffer::Buffer;

        Buffer::new(self.make(), 512).boxed_clone()
    }
}

#[cfg(test)]
mod tests {
    use http::Uri;
    use serde_json_bytes::json;

    use crate::{plugin::test::MockSupergraphService, Context};

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

    #[tokio::test]
    async fn it_extracts_query_and_operation_name() {
        let query = "query";
        let expected_query = query;
        let operation_name = "operationName";
        let expected_operation_name = operation_name;

        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();

        let mut supergraph_service = MockSupergraphService::new();

        supergraph_service
            .expect_call()
            .times(2)
            .returning(move |req| {
                let example_response = example_response.clone();

                assert_eq!(
                    req.supergraph_request.body().query.as_deref().unwrap(),
                    expected_query
                );
                assert_eq!(
                    req.supergraph_request
                        .body()
                        .operation_name
                        .as_deref()
                        .unwrap(),
                    expected_operation_name
                );
                Ok(
                    SupergraphResponse::new_from_graphql_response(example_response, Context::new())
                        .into(),
                )
            });

        let mut router_service =
            RouterCreator::new(SupergraphCreator::for_tests(supergraph_service)).make();

        let get_path = format!(
            "/?{}",
            serde_urlencoded::to_string(&[("query", query), ("operationName", operation_name)])
                .unwrap(),
        );

        let get_uri = Uri::builder().path_and_query(get_path).build().unwrap();

        let get_request = http::Request::builder()
            .method(Method::GET)
            .uri(get_uri)
            .body(hyper::Body::empty())
            .unwrap();

        let response = router_service.call(get_request.into()).await.unwrap();

        panic!("no");

        // assert_eq!(response.response.into_body(), expected_response);
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_post_requests() {
        todo!();
        // let query = "query";
        // let expected_query = query;
        // let operation_name = "operationName";
        // let expected_operation_name = operation_name;

        // let expected_response = graphql::Response::builder()
        //     .data(json!({"response": "yay"}))
        //     .build();
        // let example_response = expected_response.clone();

        // let mut expectations = MockRouterService::new();
        // expectations
        //     .expect_service_call()
        //     .times(1)
        //     .returning(move |req| {
        //         let example_response = example_response.clone();
        //         Box::pin(async move {
        //             let request: graphql::Request = serde_json::from_slice(
        //                 hyper::body::to_bytes(req.router_request.into_body())
        //                     .await
        //                     .unwrap()
        //                     .to_vec()
        //                     .as_slice(),
        //             )
        //             .unwrap();
        //             assert_eq!(request.query.as_deref().unwrap(), expected_query);
        //             assert_eq!(
        //                 request.operation_name.as_deref().unwrap(),
        //                 expected_operation_name
        //             );
        //             Ok(SupergraphResponse::new_from_graphql_response(
        //                 example_response,
        //                 Context::new(),
        //             )
        //             .into())
        //         })
        //     });
        // let (server, client) = init(expectations).await;
        // let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

        // let response = client
        //     .post(url.as_str())
        //     .body(json!({ "query": query, "operationName": operation_name }).to_string())
        //     .send()
        //     .await
        //     .unwrap()
        //     .error_for_status()
        //     .unwrap();

        // assert_eq!(
        //     response.json::<graphql::Response>().await.unwrap(),
        //     expected_response,
        // );

        // server.shutdown().await
    }
}
