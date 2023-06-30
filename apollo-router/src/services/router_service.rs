//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use axum::body::StreamBody;
use axum::response::*;
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
use router_bridge::planner::Planner;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use super::layers::apq::APQLayer;
use super::layers::content_negociation;
use super::layers::query_analysis::QueryAnalysisLayer;
use super::layers::static_page::StaticPageLayer;
use super::new_service::ServiceFactory;
use super::router;
use super::router::ClientRequestAccepts;
#[cfg(test)]
use super::supergraph;
use super::HasPlugins;
#[cfg(test)]
use super::HasSchema;
use super::SupergraphCreator;
use super::MULTIPART_DEFER_CONTENT_TYPE;
use super::MULTIPART_SUBSCRIPTION_CONTENT_TYPE;
use crate::cache::DeduplicatingCache;
use crate::graphql;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::protocols::multipart::Multipart;
use crate::protocols::multipart::ProtocolMode;
use crate::query_planner::QueryPlanResult;
use crate::router_factory::RouterFactory;
use crate::services::layers::content_negociation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::Configuration;
use crate::Endpoint;
use crate::ListenAddr;

#[cfg(test)]
mod tests;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct RouterService {
    supergraph_creator: Arc<SupergraphCreator>,
    apq_layer: APQLayer,
    query_analysis_layer: QueryAnalysisLayer,
    experimental_http_max_request_bytes: usize,
}

impl RouterService {
    pub(crate) fn new(
        supergraph_creator: Arc<SupergraphCreator>,
        apq_layer: APQLayer,
        query_analysis_layer: QueryAnalysisLayer,
        experimental_http_max_request_bytes: usize,
    ) -> Self {
        RouterService {
            supergraph_creator,
            apq_layer,
            query_analysis_layer,
            experimental_http_max_request_bytes,
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

    let (_, supergraph_creator) = crate::TestHarness::builder()
        .configuration(configuration.clone())
        .supergraph_hook(move |_| supergraph_service.clone().boxed())
        .build_common()
        .await
        .unwrap();

    RouterCreator::new(
        QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&configuration)).await,
        Arc::new(supergraph_creator),
        configuration,
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

    let (_, supergraph_creator) = crate::TestHarness::builder()
        .configuration(Default::default())
        .supergraph_hook(move |_| supergraph_service.clone().boxed())
        .build_common()
        .await
        .unwrap();

    RouterCreator::new(
        QueryAnalysisLayer::new(supergraph_creator.schema(), Default::default()).await,
        Arc::new(supergraph_creator),
        Default::default(),
    )
    .await
    .make()
}

impl Service<RouterRequest> for RouterService {
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
        let query_analysis = self.query_analysis_layer.clone();
        let experimental_http_max_request_bytes = self.experimental_http_max_request_bytes;

        let fut = async move {
            let graphql_request: Result<graphql::Request, (StatusCode, &str, String)> = if parts
                .method
                == Method::GET
            {
                parts
                    .uri
                    .query()
                    .map(|q| {
                        graphql::Request::from_urlencoded_query(q.to_string()).map_err(|e| {
                            (
                                StatusCode::BAD_REQUEST,
                                "failed to decode a valid GraphQL request from path",
                                format!("failed to decode a valid GraphQL request from path {e}"),
                            )
                        })
                    })
                    .unwrap_or_else(|| {
                        Err((
                            StatusCode::BAD_REQUEST,
                            "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.", 
                            "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.".to_string()
                        ))
                    })
            } else {
                // FIXME: use a try block when available: https://github.com/rust-lang/rust/issues/31436
                let content_length = (|| {
                    parts
                        .headers
                        .get(http::header::CONTENT_LENGTH)?
                        .to_str()
                        .ok()?
                        .parse()
                        .ok()
                })();
                if content_length.unwrap_or(0) > experimental_http_max_request_bytes {
                    Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "payload too large for the `experimental_http_max_request_bytes` configuration",
                        "payload too large".to_string(),
                    ))
                } else {
                    let body = http_body::Limited::new(body, experimental_http_max_request_bytes);
                    hyper::body::to_bytes(body)
                        .instrument(tracing::debug_span!("receive_body"))
                        .await
                        .map_err(|e| {
                            if e.is::<http_body::LengthLimitError>() {
                                (
                                    StatusCode::PAYLOAD_TOO_LARGE,
                                    "payload too large for the `experimental_http_max_request_bytes` configuration",
                                    "payload too large".to_string(),
                                )
                            } else {
                                (
                                    StatusCode::BAD_REQUEST,
                                    "failed to get the request body",
                                    format!("failed to get the request body: {e}"),
                                )
                            }
                        })
                        .and_then(|bytes| {
                            graphql::Request::deserialize_from_bytes(&bytes).map_err(|err| {
                                (
                                    StatusCode::BAD_REQUEST,
                                    "failed to deserialize the request body into JSON",
                                    format!(
                                        "failed to deserialize the request body into JSON: {err}"
                                    ),
                                )
                            })
                        })
                }
            };

            match graphql_request {
                Ok(graphql_request) => {
                    let request = SupergraphRequest {
                        supergraph_request: http::Request::from_parts(parts, graphql_request),
                        context,
                        compiler: None,
                    };

                    let request_res = apq.supergraph_request(request).await;
                    let SupergraphResponse { response, context } = match request_res {
                        Err(response) => response,
                        Ok(request) => match query_analysis.supergraph_request(request).await {
                            Err(response) => response,
                            Ok(request) => supergraph_creator.create().oneshot(request).await?,
                        },
                    };

                    let ClientRequestAccepts {
                        wildcard: accepts_wildcard,
                        json: accepts_json,
                        multipart_defer: accepts_multipart_defer,
                        multipart_subscription: accepts_multipart_subscription,
                    } = context
                        .private_entries
                        .lock()
                        .get()
                        .cloned()
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
                                && !response.subscribed.unwrap_or(false)
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
                            } else if accepts_multipart_defer || accepts_multipart_subscription {
                                if accepts_multipart_defer {
                                    parts.headers.insert(
                                        CONTENT_TYPE,
                                        HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
                                    );
                                } else if accepts_multipart_subscription {
                                    parts.headers.insert(
                                        CONTENT_TYPE,
                                        HeaderValue::from_static(
                                            MULTIPART_SUBSCRIPTION_CONTENT_TYPE,
                                        ),
                                    );
                                }
                                let multipart_stream = match response.subscribed {
                                    Some(true) => StreamBody::new(Multipart::new(
                                        body,
                                        ProtocolMode::Subscription,
                                    )),
                                    _ => StreamBody::new(Multipart::new(
                                        once(ready(response)).chain(body),
                                        ProtocolMode::Defer,
                                    )),
                                };

                                let response =
                                    (parts, multipart_stream).into_response().map(|body| {
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
                                router::Response::error_builder()
                                    .error(
                                        graphql::Error::builder()
                                            .message(format!(
                                                r#"'accept' header must be one of: \"*/*\", {:?}, {:?} or {:?}"#,
                                                APPLICATION_JSON.essence_str(),
                                                GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                                MULTIPART_DEFER_CONTENT_TYPE
                                            ))
                                            .extension_code("INVALID_ACCEPT_HEADER")
                                            .build(),
                                    )
                                    .status_code(StatusCode::NOT_ACCEPTABLE)
                                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                                    .context(context)
                                    .build()
                            }
                        }
                    }
                }
                Err((status_code, error, extension_details)) => {
                    ::tracing::error!(
                        monotonic_counter.apollo_router_http_requests_total = 1u64,
                        status = %status_code.as_u16(),
                        error = %error,
                        %error
                    );

                    router::Response::error_builder()
                        .error(
                            graphql::Error::builder()
                                .message(String::from("Invalid GraphQL request"))
                                .extension_code("INVALID_GRAPHQL_REQUEST")
                                .extension("details", extension_details)
                                .build(),
                        )
                        .status_code(status_code)
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .context(context)
                        .build()
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
pub(crate) struct RouterCreator {
    supergraph_creator: Arc<SupergraphCreator>,
    static_page: StaticPageLayer,
    apq_layer: APQLayer,
    query_analysis_layer: QueryAnalysisLayer,
    experimental_http_max_request_bytes: usize,
}

impl ServiceFactory<router::Request> for RouterCreator {
    type Service = router::BoxService;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl RouterFactory for RouterCreator {
    type RouterService = router::BoxService;

    type Future = <<RouterCreator as ServiceFactory<router::Request>>::Service as Service<
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

impl RouterCreator {
    pub(crate) async fn new(
        query_analysis_layer: QueryAnalysisLayer,
        supergraph_creator: Arc<SupergraphCreator>,
        configuration: Arc<Configuration>,
    ) -> Self {
        let static_page = StaticPageLayer::new(&configuration);
        let apq_layer = if configuration.apq.enabled {
            APQLayer::with_cache(
                DeduplicatingCache::from_configuration(&configuration.apq.router.cache, "APQ")
                    .await,
            )
        } else {
            APQLayer::disabled()
        };

        Self {
            supergraph_creator,
            static_page,
            apq_layer,
            query_analysis_layer,
            experimental_http_max_request_bytes: configuration
                .preview_operation_limits
                .experimental_http_max_request_bytes,
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
            self.query_analysis_layer.clone(),
            self.experimental_http_max_request_bytes,
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

impl RouterCreator {
    pub(crate) async fn cache_keys(&self, count: usize) -> Vec<(String, Option<String>)> {
        self.supergraph_creator.cache_keys(count).await
    }

    pub(crate) fn planner(&self) -> Arc<Planner<QueryPlanResult>> {
        self.supergraph_creator.planner()
    }
}
