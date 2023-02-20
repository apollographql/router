//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
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
use http::StatusCode;
use http_body::Body as _;
use hyper::Body;
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use super::layers::apq::APQLayer;
use super::layers::content_negociation;
use super::layers::content_negociation::ACCEPTS_JSON_CONTEXT_KEY;
use super::layers::content_negociation::ACCEPTS_MULTIPART_CONTEXT_KEY;
use super::layers::content_negociation::ACCEPTS_WILDCARD_CONTEXT_KEY;
use super::layers::static_page::StaticPageLayer;
use super::new_service::ServiceFactory;
use super::router;
use super::supergraph;
use super::HasPlugins;
#[cfg(test)]
use super::SupergraphCreator;
use super::MULTIPART_DEFER_CONTENT_TYPE;
use crate::cache::DeduplicatingCache;
use crate::graphql;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::router_factory::RouterFactory;
use crate::services::layers::content_negociation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::Configuration;
use crate::Endpoint;
use crate::ListenAddr;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    supergraph_creator: Arc<SF>,
    apq_layer: APQLayer,
}

impl<SF> RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(supergraph_creator: Arc<SF>, apq_layer: APQLayer) -> Self {
        RouterService {
            supergraph_creator,
            apq_layer,
        }
    }
}

#[cfg(test)]
pub(crate) async fn from_supergraph_mock_callback_and_configuration(
    supergraph_callback: impl FnMut(supergraph::Request) -> supergraph::ServiceResult
        + Send
        + Sync
        + 'static
        + Clone,
    configuration: Arc<Configuration>,
) -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    let mut supergraph_service = MockSupergraphService::new();

    supergraph_service.expect_clone().returning(move || {
        let cloned_callback = supergraph_callback.clone();
        let mut supergraph_service = MockSupergraphService::new();
        supergraph_service.expect_call().returning(cloned_callback);
        supergraph_service
    });

    RouterCreator::new(
        Arc::new(SupergraphCreator::for_tests(supergraph_service).await),
        &configuration,
    )
    .await
    .make()
}

#[cfg(test)]
pub(crate) async fn from_supergraph_mock_callback(
    supergraph_callback: impl FnMut(supergraph::Request) -> supergraph::ServiceResult
        + Send
        + Sync
        + 'static
        + Clone,
) -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    from_supergraph_mock_callback_and_configuration(
        supergraph_callback,
        Arc::new(Configuration::default()),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn empty() -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    let mut supergraph_service = MockSupergraphService::new();
    supergraph_service
        .expect_clone()
        .returning(MockSupergraphService::new);

    RouterCreator::new(
        Arc::new(SupergraphCreator::for_tests(supergraph_service).await),
        &Configuration::default(),
    )
    .await
    .make()
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

        let supergraph_creator = self.supergraph_creator.clone();
        let apq = self.apq_layer.clone();

        let fut = async move {
            let graphql_request: Result<graphql::Request, (&str, String)> = if parts.method
                == Method::GET
            {
                parts
                    .uri
                    .query()
                    .map(|q| {
                        graphql::Request::from_urlencoded_query(q.to_string()).map_err(|e| {
                            (
                                "failed to decode a valid GraphQL request from path",
                                format!("failed to decode a valid GraphQL request from path {e}"),
                            )
                        })
                    })
                    .unwrap_or_else(|| {
                        Err(("missing query string", "missing query string".to_string()))
                    })
            } else {
                hyper::body::to_bytes(body)
                    .await
                    .map_err(|e| {
                        (
                            "failed to get the request body",
                            format!("failed to get the request body: {e}"),
                        )
                    })
                    .and_then(|bytes| {
                        serde_json::from_reader(bytes.reader()).map_err(|err| {
                            (
                                "failed to deserialize the request body into JSON",
                                format!("failed to deserialize the request body into JSON: {err}"),
                            )
                        })
                    })
            };

            match graphql_request {
                Ok(graphql_request) => {
                    let request = SupergraphRequest {
                        supergraph_request: http::Request::from_parts(parts, graphql_request),
                        context,
                    };

                    let request_res = apq.supergraph_request(request).await;

                    let SupergraphResponse { response, context } =
                        match request_res.and_then(|request| {
                            let query = request.supergraph_request.body().query.as_ref();

                            if query.is_none() || query.unwrap().trim().is_empty() {
                                let errors = vec![crate::error::Error::builder()
                                    .message("Must provide query string.".to_string())
                                    .extension_code("MISSING_QUERY_STRING")
                                    .build()];
                                tracing::error!(
                                    monotonic_counter.apollo_router_http_requests_total = 1u64,
                                    status = %StatusCode::BAD_REQUEST.as_u16(),
                                    error = "Must provide query string",
                                    "Must provide query string"
                                );

                                Err(SupergraphResponse::builder()
                                    .errors(errors)
                                    .status_code(StatusCode::BAD_REQUEST)
                                    .context(request.context)
                                    .build()
                                    .expect("response is valid"))
                            } else {
                                Ok(request)
                            }
                        }) {
                            Err(response) => response,
                            Ok(request) => supergraph_creator.create().oneshot(request).await?,
                        };

                    let accepts_wildcard: bool = context
                        .get(ACCEPTS_WILDCARD_CONTEXT_KEY)
                        .unwrap_or_default()
                        .unwrap_or_default();
                    let accepts_json: bool = context
                        .get(ACCEPTS_JSON_CONTEXT_KEY)
                        .unwrap_or_default()
                        .unwrap_or_default();
                    let accepts_multipart: bool = context
                        .get(ACCEPTS_MULTIPART_CONTEXT_KEY)
                        .unwrap_or_default()
                        .unwrap_or_default();

                    let (mut parts, mut body) = response.into_parts();
                    process_vary_header(&mut parts.headers);

                    match body.next().await {
                        None => {
                            tracing::error!("router service is not available to process request",);
                            Ok(router::Response {
                                response: http::Response::builder()
                                    .status(StatusCode::SERVICE_UNAVAILABLE)
                                    .body(Body::from(
                                        "router service is not available to process request",
                                    ))
                                    .expect("cannot fail"),
                                context,
                            })
                        }
                        Some(response) => {
                            if !response.has_next.unwrap_or(false)
                                && (accepts_json || accepts_wildcard)
                            {
                                parts.headers.insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static(APPLICATION_JSON.essence_str()),
                                );
                                tracing::trace_span!("serialize_response").in_scope(|| {
                                    let body = serde_json::to_string(&response)?;
                                    Ok(router::Response {
                                        response: http::Response::from_parts(
                                            parts,
                                            Body::from(body),
                                        ),
                                        context,
                                    })
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
                                serde_json::to_writer(&mut first_buf, &response)?;
                                if response.has_next.unwrap_or(false) {
                                    first_buf.extend_from_slice(b"\r\n--graphql\r\n");
                                } else {
                                    first_buf.extend_from_slice(b"\r\n--graphql--\r\n");
                                }

                                let body = once(ready(Ok(Bytes::from(first_buf)))).chain(body.map(
                                    |res| {
                                        let mut buf = Vec::from(
                                            &b"content-type: application/json\r\n\r\n"[..],
                                        );
                                        serde_json::to_writer(&mut buf, &res)?;

                                        // the last chunk has a different end delimiter
                                        if res.has_next.unwrap_or(false) {
                                            buf.extend_from_slice(b"\r\n--graphql\r\n");
                                        } else {
                                            buf.extend_from_slice(b"\r\n--graphql--\r\n");
                                        }

                                        Ok::<_, BoxError>(buf.into())
                                    },
                                ));

                                let response =
                                    (parts, StreamBody::new(body)).into_response().map(|body| {
                                        // Axum makes this `body` have type:
                                        // https://docs.rs/http-body/0.4.5/http_body/combinators/struct.UnsyncBoxBody.html
                                        let mut body = Box::pin(body);
                                        // We make a stream based on its `poll_data` method
                                        // in order to create a `hyper::Body`.
                                        Body::wrap_stream(stream::poll_fn(move |ctx| {
                                            body.as_mut().poll_data(ctx)
                                        }))
                                        // … but we ignore the `poll_trailers` method:
                                        // https://docs.rs/http-body/0.4.5/http_body/trait.Body.html#tymethod.poll_trailers
                                        // Apparently HTTP/2 trailers are like headers, except after the response body.
                                        // I (Simon) believe nothing in the Apollo Router uses trailers as of this writing,
                                        // so ignoring `poll_trailers` is fine.
                                        // If we want to use trailers, we may need remove this convertion to `hyper::Body`
                                        // and return `UnsyncBoxBody` (a.k.a. `axum::BoxBody`) as-is.
                                    });

                                Ok(RouterResponse { response, context })
                            } else {
                                // this should be unreachable due to a previous check, but just to be sure...
                                Ok(router::Response {
                                response: http::Response::builder()
                                    .status(StatusCode::NOT_ACCEPTABLE)
                                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                                    .body(
                                    Body::from(
                                        serde_json::to_string(
                                            &graphql::Error::builder()
                                                .message(format!(
                                                    r#"'accept' header can't be different from \"*/*\", {:?}, {:?} or {:?}"#,
                                                    APPLICATION_JSON.essence_str(),
                                                    GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                                    MULTIPART_DEFER_CONTENT_TYPE
                                                ))
                                                .extension_code("INVALID_ACCEPT_HEADER")
                                                .build(),
                                        )
                                        .unwrap_or_else(|_| String::from("Invalid request"))
                                    )
                                ).expect("cannot fail"),
                                context,
                            })
                            }
                        }
                    }
                }
                Err((error, extension_details)) => {
                    // BAD REQUEST
                    ::tracing::error!(
                        monotonic_counter.apollo_router_http_requests_total = 1u64,
                        status = %400,
                        error = %error,
                        %error
                    );
                    Ok(router::Response {
                        response: http::Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header(CONTENT_TYPE, APPLICATION_JSON.to_string())
                            .body(Body::from(
                                serde_json::to_string(
                                    &graphql::Error::builder()
                                        .message(String::from("Invalid GraphQL request"))
                                        .extension_code("INVALID_GRAPHQL_REQUEST")
                                        .extension("details", extension_details)
                                        .build(),
                                )
                                .unwrap_or_else(|_| String::from("Invalid GraphQL request")),
                            ))
                            .expect("cannot fail"),
                        context,
                    })
                }
            }
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
    supergraph_creator: Arc<SF>,
    static_page: StaticPageLayer,
    apq_layer: APQLayer,
}

impl<SF> ServiceFactory<router::Request> for RouterCreator<SF>
where
    SF: HasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
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
    SF: HasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
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
    SF: HasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    pub(crate) async fn new(supergraph_creator: Arc<SF>, configuration: &Configuration) -> Self {
        let static_page = StaticPageLayer::new(configuration);
        let apq_layer = if configuration.supergraph.apq.enabled {
            APQLayer::with_cache(
                DeduplicatingCache::from_configuration(
                    &configuration.supergraph.apq.experimental_cache,
                    "APQ",
                )
                .await,
            )
        } else {
            APQLayer::disabled()
        };

        Self {
            supergraph_creator,
            static_page,
            apq_layer,
        }
    }

    pub(crate) fn make(
        &self,
    ) -> impl Service<
        router::Request,
        Response = router::Response,
        Error = BoxError,
        Future = BoxFuture<'static, router::ServiceResult>,
    > + Send {
        let router_service = content_negociation::RouterLayer::default().layer(RouterService::new(
            self.supergraph_creator.clone(),
            self.apq_layer.clone(),
        ));

        ServiceBuilder::new()
            .layer(self.static_page.clone())
            .service(
                self.supergraph_creator
                    .plugins()
                    .iter()
                    .rev()
                    .fold(router_service.boxed(), |acc, (_, e)| e.router_service(acc)),
            )
    }
}

impl RouterCreator<crate::services::supergraph_service::SupergraphCreator> {
    pub(crate) async fn cache_keys(&self, count: usize) -> Vec<(String, Option<String>)> {
        self.supergraph_creator.cache_keys(count).await
    }
}

#[cfg(test)]
mod tests {
    use http::Uri;
    use mime::APPLICATION_JSON;
    use serde_json_bytes::json;

    use super::*;
    use crate::services::supergraph;
    use crate::Context;

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

        let mut router_service = super::from_supergraph_mock_callback(move |req| {
            let example_response = expected_response.clone();

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

            Ok(SupergraphResponse::new_from_graphql_response(
                example_response,
                req.context,
            ))
        })
        .await;

        // get request
        let get_request = supergraph::Request::builder()
            .query(query)
            .operation_name(operation_name)
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .uri(Uri::from_static("/"))
            .method(Method::GET)
            .context(Context::new())
            .build()
            .unwrap()
            .try_into()
            .unwrap();

        router_service.call(get_request).await.unwrap();

        // post request
        let post_request = supergraph::Request::builder()
            .query(query)
            .operation_name(operation_name)
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .uri(Uri::from_static("/"))
            .method(Method::POST)
            .context(Context::new())
            .build()
            .unwrap();

        router_service
            .call(post_request.try_into().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_fails_on_empty_query() {
        let expected_error = "Must provide query string.";

        let router_service = from_supergraph_mock_callback(move |_req| unreachable!()).await;

        let request = SupergraphRequest::fake_builder()
            .query("".to_string())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let response = router_service
            .oneshot(request)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();
        let actual_error = response.errors[0].message.clone();

        assert_eq!(expected_error, actual_error);
        assert!(response.errors[0].extensions.contains_key("code"));
    }

    #[tokio::test]
    async fn it_fails_on_no_query() {
        let expected_error = "Must provide query string.";

        let router_service = from_supergraph_mock_callback(move |_req| unreachable!()).await;

        let request = SupergraphRequest::fake_builder()
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let response = router_service
            .oneshot(request)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();
        let actual_error = response.errors[0].message.clone();
        assert_eq!(expected_error, actual_error);
        assert!(response.errors[0].extensions.contains_key("code"));
    }
}
