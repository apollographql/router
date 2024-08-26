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
use http::request::Parts;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http_body::Body as _;
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use serde::de::Error;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use crate::axum_factory::CanceledRequest;
use crate::cache::DeduplicatingCache;
use crate::configuration::Batching;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::graphql;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::protocols::multipart::Multipart;
use crate::protocols::multipart::ProtocolMode;
use crate::query_planner::InMemoryCachePlanner;
use crate::router_factory::RouterFactory;
use crate::services::layers::apq::APQLayer;
use crate::services::layers::content_negotiation;
use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::layers::static_page::StaticPageLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router::body::get_body_bytes;
use crate::services::router::body::RouterBody;
#[cfg(test)]
use crate::services::supergraph;
use crate::services::HasPlugins;
#[cfg(test)]
use crate::services::HasSchema;
use crate::services::JsonRequest;
use crate::services::JsonResponse;
use crate::services::JsonServerService;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SupergraphCreator;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::MULTIPART_DEFER_ACCEPT;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::services::MULTIPART_SUBSCRIPTION_ACCEPT;
use crate::services::MULTIPART_SUBSCRIPTION_CONTENT_TYPE;
use crate::Configuration;
use crate::Endpoint;
use crate::ListenAddr;

pub(crate) static MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE);
pub(crate) static MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static(MULTIPART_SUBSCRIPTION_CONTENT_TYPE);
static ACCEL_BUFFERING_HEADER_NAME: HeaderName = HeaderName::from_static("x-accel-buffering");
static ACCEL_BUFFERING_HEADER_VALUE: HeaderValue = HeaderValue::from_static("no");
static ORIGIN_HEADER_VALUE: HeaderValue = HeaderValue::from_static("origin");

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct RouterService {
    supergraph_creator: Arc<SupergraphCreator>,
    apq_layer: APQLayer,
    persisted_query_layer: Arc<PersistedQueryLayer>,
    query_analysis_layer: QueryAnalysisLayer,
    http_max_request_bytes: usize,
    batching: Batching,
}

impl RouterService {
    pub(crate) fn new(
        supergraph_creator: Arc<SupergraphCreator>,
        apq_layer: APQLayer,
        persisted_query_layer: Arc<PersistedQueryLayer>,
        query_analysis_layer: QueryAnalysisLayer,
        http_max_request_bytes: usize,
        batching: Batching,
    ) -> Self {
        RouterService {
            supergraph_creator,
            apq_layer,
            persisted_query_layer,
            query_analysis_layer,
            http_max_request_bytes,
            batching,
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
        Arc::new(PersistedQueryLayer::new(&configuration).await.unwrap()),
        Arc::new(supergraph_creator),
        configuration,
    )
    .await
    .unwrap()
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
        Arc::new(PersistedQueryLayer::new(&Default::default()).await.unwrap()),
        Arc::new(supergraph_creator),
        Arc::new(Configuration::default()),
    )
    .await
    .unwrap()
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
        let clone = self.clone();

        let this = std::mem::replace(self, clone);

        let fut = async move { this.call_inner(req).await };
        Box::pin(fut)
    }
}

impl RouterService {
    async fn call_inner(&self, req: RouterRequest) -> Result<RouterResponse, BoxError> {
        let context = req.context.clone();
        let json_request = match self.translate_json_request(req).await {
            Ok(request) => request,
            Err(err) => {
                u64_counter!(
                    "apollo_router_http_requests_total",
                    "Total number of HTTP requests made.",
                    1,
                    status = err.status.as_u16() as i64,
                    error = err.error.to_string()
                );
                // Useful for selector in spans/instruments/events
                context
                    .insert_json_value(CONTAINS_GRAPHQL_ERROR, serde_json_bytes::Value::Bool(true));
                return router::Response::error_builder()
                    .error(
                        graphql::Error::builder()
                            .message(String::from("Invalid GraphQL request"))
                            .extension_code(err.extension_code)
                            .extension("details", err.extension_details)
                            .build(),
                    )
                    .status_code(err.status)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .context(context)
                    .build();
            }
        };

        self.process_json_request(json_request).await
    }

    async fn translate_json_request(
        &self,
        req: RouterRequest,
    ) -> Result<JsonRequest, TranslateError> {
        let RouterRequest {
            router_request,
            context,
        } = req;

        let (parts, body) = router_request.into_parts();
        if parts.method == Method::GET {
            let value = self.translate_query_request(&parts).await?;
            return Ok(JsonRequest {
                request: http::Request::from_parts(parts, value),
                context,
            });
        }

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
        if content_length.unwrap_or(0) > self.http_max_request_bytes {
            Err(TranslateError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                error: "payload too large for the `http_max_request_bytes` configuration",
                extension_code: "INVALID_GRAPHQL_REQUEST",
                extension_details: "payload too large".to_string(),
            })
        } else {
            let body = http_body::Limited::new(body, self.http_max_request_bytes);
            let bytes = get_body_bytes(body)
                .instrument(tracing::debug_span!("receive_body"))
                .await
                .map_err(|e| {
                    if e.is::<http_body::LengthLimitError>() {
                        TranslateError {
                            status: StatusCode::PAYLOAD_TOO_LARGE,
                            error:
                                "payload too large for the `http_max_request_bytes` configuration",
                            extension_code: "INVALID_GRAPHQL_REQUEST",
                            extension_details: "payload too large".to_string(),
                        }
                    } else {
                        TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            error: "failed to get the request body",
                            extension_code: "INVALID_GRAPHQL_REQUEST",
                            extension_details: format!("failed to get the request body: {e}"),
                        }
                    }
                })?;

            let value = serde_json_bytes::Value::from_bytes(bytes).map_err(|e| TranslateError {
                status: StatusCode::BAD_REQUEST,
                error: "failed to deserialize the request body into JSON",
                extension_code: "INVALID_GRAPHQL_REQUEST",
                extension_details: format!("failed to deserialize the request body into JSON: {e}"),
            })?;

            let request = http::Request::from_parts(parts, value);
            Ok(JsonRequest { request, context })
        }
    }

    async fn translate_query_request(&self, parts: &Parts) -> Result<Value, TranslateError> {
        parts.uri.query().map(|q| {
            serde_urlencoded::from_str(q)
            .map_err(serde_json::Error::custom).map_err(|e| {

                TranslateError {
                    status: StatusCode::BAD_REQUEST,
                    error: "failed to deserialize the request body into JSON",
                    extension_code: "INVALID_GRAPHQL_REQUEST",
                    extension_details: format!(
                        "failed to deserialize the request body into JSON: {e}"
                    ),
                }
             })
        }).unwrap_or_else(|| {

            Err(TranslateError {
                status: StatusCode::BAD_REQUEST,
                error: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.",
                extension_code: "INVALID_GRAPHQL_REQUEST",
                extension_details: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.".to_string()
            })
        })
    }

    async fn process_json_request(
        &self,
        json_request: JsonRequest,
    ) -> Result<router::Response, BoxError> {
        let mut service = JsonServerService::new(
            self.supergraph_creator.clone(),
            self.apq_layer.clone(),
            self.persisted_query_layer.clone(),
            self.query_analysis_layer.clone(),
            self.batching.clone(),
        );

        let JsonResponse { response, context } = service.call(json_request).await?;

        let (mut parts, mut body) = response.into_parts();
        process_vary_header(&mut parts.headers);

        if context
            .extensions()
            .with_lock(|lock| lock.get::<CanceledRequest>().is_some())
        {
            parts.status = StatusCode::from_u16(499)
                .expect("499 is not a standard status code but common enough");
        }

        match body.next().await {
            None => {
                tracing::error!("router service is not available to process request",);
                Ok(router::Response {
                    response: http::Response::builder()
                        .status(StatusCode::SERVICE_UNAVAILABLE)
                        .body(
                            RouterBody::from("router service is not available to process request")
                                .into_inner(),
                        )
                        .expect("cannot fail"),
                    context,
                })
            }
            Some(response) => {
                if parts.headers.get(CONTENT_TYPE) == Some(&APPLICATION_JSON_HEADER_VALUE) {
                    tracing::trace_span!("serialize_response").in_scope(|| {
                        let body = serde_json::to_string(&response)?;
                        Ok(router::Response {
                            response: http::Response::from_parts(
                                parts,
                                RouterBody::from(body).into_inner(),
                            ),
                            context,
                        })
                    })
                } else if parts.headers.get(CONTENT_TYPE)
                    == Some(&MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE)
                    || parts.headers.get(CONTENT_TYPE)
                        == Some(&MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE)
                {
                    // Useful when you're using a proxy like nginx which enable proxy_buffering by default (http://nginx.org/en/docs/http/ngx_http_proxy_module.html#proxy_buffering)
                    parts.headers.insert(
                        ACCEL_BUFFERING_HEADER_NAME.clone(),
                        ACCEL_BUFFERING_HEADER_VALUE.clone(),
                    );
                    let multipart_stream = if parts.headers.get(CONTENT_TYPE)
                        == Some(&MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE)
                    {
                        StreamBody::new(Multipart::new(body, ProtocolMode::Subscription))
                    } else {
                        StreamBody::new(Multipart::new(
                            once(ready(response)).chain(body),
                            ProtocolMode::Defer,
                        ))
                    };
                    let response = (parts, multipart_stream).into_response().map(|body| {
                        // Axum makes this `body` have type:
                        // https://docs.rs/http-body/0.4.5/http_body/combinators/struct.UnsyncBoxBody.html
                        let mut body = Box::pin(body);
                        // We make a stream based on its `poll_data` method
                        // in order to create a `hyper::Body`.
                        RouterBody::wrap_stream(stream::poll_fn(move |ctx| {
                            body.as_mut().poll_data(ctx)
                        }))
                        .into_inner()
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
                    tracing::info!(
                        monotonic_counter.apollo.router.graphql_error = 1u64,
                        code = "INVALID_ACCEPT_HEADER"
                    );
                    // Useful for selector in spans/instruments/events
                    context.insert_json_value(
                        CONTAINS_GRAPHQL_ERROR,
                        serde_json_bytes::Value::Bool(true),
                    );

                    // this should be unreachable due to a previous check, but just to be sure...
                    Ok(router::Response::error_builder()
                            .error(
                                graphql::Error::builder()
                                    .message(format!(
                                        r#"'accept' header must be one of: \"*/*\", {:?}, {:?}, {:?} or {:?}"#,
                                        APPLICATION_JSON.essence_str(),
                                        GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                        MULTIPART_DEFER_ACCEPT,
                                        MULTIPART_SUBSCRIPTION_ACCEPT,
                                    ))
                                    .extension_code("INVALID_ACCEPT_HEADER")
                                    .build(),
                            )
                            .status_code(StatusCode::NOT_ACCEPTABLE)
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .context(context)
                            .build()?)
                }
            }
        }
    }
}

struct TranslateError<'a> {
    status: StatusCode,
    error: &'a str,
    extension_code: &'a str,
    extension_details: String,
}

// Process the headers to make sure that `VARY` is set correctly
pub(crate) fn process_vary_header(headers: &mut HeaderMap<HeaderValue>) {
    if headers.get(VARY).is_none() {
        // We don't have a VARY header, add one with value "origin"
        headers.insert(VARY, ORIGIN_HEADER_VALUE.clone());
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct RouterCreator {
    pub(crate) supergraph_creator: Arc<SupergraphCreator>,
    static_page: StaticPageLayer,
    apq_layer: APQLayer,
    pub(crate) persisted_query_layer: Arc<PersistedQueryLayer>,
    query_analysis_layer: QueryAnalysisLayer,
    http_max_request_bytes: usize,
    batching: Batching,
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
        persisted_query_layer: Arc<PersistedQueryLayer>,
        supergraph_creator: Arc<SupergraphCreator>,
        configuration: Arc<Configuration>,
    ) -> Result<Self, BoxError> {
        let static_page = StaticPageLayer::new(&configuration);
        let apq_layer = if configuration.apq.enabled {
            APQLayer::with_cache(
                DeduplicatingCache::from_configuration(&configuration.apq.router.cache, "APQ")
                    .await?,
            )
        } else {
            APQLayer::disabled()
        };

        Ok(Self {
            supergraph_creator,
            static_page,
            apq_layer,
            query_analysis_layer,
            http_max_request_bytes: configuration.limits.http_max_request_bytes,
            persisted_query_layer,
            batching: configuration.batching.clone(),
        })
    }

    pub(crate) fn make(
        &self,
    ) -> impl Service<
        router::Request,
        Response = router::Response,
        Error = BoxError,
        Future = BoxFuture<'static, router::ServiceResult>,
    > + Send {
        let router_service = content_negotiation::RouterLayer::default().layer(RouterService::new(
            self.supergraph_creator.clone(),
            self.apq_layer.clone(),
            self.persisted_query_layer.clone(),
            self.query_analysis_layer.clone(),
            self.http_max_request_bytes,
            self.batching.clone(),
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
    pub(crate) fn previous_cache(&self) -> InMemoryCachePlanner {
        self.supergraph_creator.previous_cache()
    }
}
