//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use axum::response::*;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::future::ready;
use futures::stream::StreamExt;
use futures::stream::once;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use http::header::VARY;
use http::request::Parts;
use http_body::Body as _;
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use super::Body;
use super::ClientRequestAccepts;
use super::tower_compat::APQCachingLayer;
use super::tower_compat::ParseQueryLayer;
use crate::Configuration;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;
use crate::axum_factory::CanceledRequest;
use crate::cache::DeduplicatingCache;
use crate::configuration::Batching;
use crate::graphql;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::ServiceBuilderExt;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_URI;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_VERSION;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::router::events::DisplayRouterRequest;
use crate::plugins::telemetry::config_new::router::events::DisplayRouterResponse;
use crate::protocols::multipart::Multipart;
use crate::protocols::multipart::ProtocolMode;
use crate::query_planner::InMemoryCachePlanner;
use crate::router_factory::RouterFactory;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::HasPlugins;
use crate::services::HasSchema;
use crate::services::MULTIPART_DEFER_ACCEPT;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::services::MULTIPART_SUBSCRIPTION_ACCEPT;
use crate::services::MULTIPART_SUBSCRIPTION_CONTENT_TYPE;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SupergraphCreator;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::layers::apq::APQLayer;
use crate::services::layers::content_negotiation;
use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::layers::persisted_queries::EnforceSafelistLayer;
use crate::services::layers::persisted_queries::ExpandIdsLayer;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::layers::static_page::StaticPageLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router::batching::BatchingLayer;
use crate::services::router::pipeline_handle::PipelineHandle;
use crate::services::router::pipeline_handle::PipelineRef;
use crate::services::supergraph;

pub(crate) static MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE);
pub(crate) static MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static(MULTIPART_SUBSCRIPTION_CONTENT_TYPE);
static ACCEL_BUFFERING_HEADER_NAME: HeaderName = HeaderName::from_static("x-accel-buffering");
static ACCEL_BUFFERING_HEADER_VALUE: HeaderValue = HeaderValue::from_static("no");
static ORIGIN_HEADER_VALUE: HeaderValue = HeaderValue::from_static("origin");

/// Containing [`Service`] in the request lifecyle.
pub(crate) struct RouterService {
    // A service stack for the actual implementation of the router service.
    service: router::BoxService,
}

impl RouterService {
    fn new(
        supergraph_service: supergraph::BoxService,
        apq_layer: APQLayer,
        persisted_query_layer: Arc<PersistedQueryLayer>,
        query_analysis_layer: QueryAnalysisLayer,
        batching: Batching,
    ) -> Self {
        // Some of the layers in the stack are wrapping previous implementations that are called
        // layers, but are not tower layers at all.
        let apq_layer = Arc::new(apq_layer);
        let query_analysis_layer = Arc::new(query_analysis_layer);

        let service = ServiceBuilder::new()
            .layer(DisplayRouterRequestLayer)
            .layer(BatchingLayer::new(batching))
            .layer(RouterToSupergraphRequestLayer)
            .layer(ExpandIdsLayer::new(persisted_query_layer.clone()))
            .layer(APQCachingLayer::new(apq_layer))
            .layer(ParseQueryLayer::new(query_analysis_layer))
            .layer(EnforceSafelistLayer::new(persisted_query_layer))
            .buffered() // Makes the supergraph service cloneable
            .service(supergraph_service)
            .boxed();

        RouterService { service }
    }
}

impl Service<RouterRequest> for RouterService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        self.service.call(req)
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

    let (_, _, supergraph_creator) = crate::TestHarness::builder()
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

    let (_, _, supergraph_creator) = crate::TestHarness::builder()
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

/// If the `DisplayRouterRequest(true)` marker value is in context,
/// reads the request body and logs it out.
struct DisplayRouterRequestLayer;
impl<S> tower::Layer<S> for DisplayRouterRequestLayer {
    type Service = DisplayRouterRequestService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        DisplayRouterRequestService { inner }
    }
}

#[derive(Clone)]
struct DisplayRouterRequestService<S> {
    inner: S,
}

impl<S> Service<RouterRequest> for DisplayRouterRequestService<S>
where
    S: Service<RouterRequest, Response = RouterResponse, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: RouterRequest) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            if let Some(level) = req
                .context
                .extensions()
                .with_lock(|ext| ext.get::<DisplayRouterRequest>().cloned())
                .map(|d| d.0)
            {
                // XXX(@goto-bus-stop): a better implementation of this might be to wrap the body
                // type, and log automatically once it is out of data. It also wouldn't require the
                // `is_fixed_size` workaround below.
                let RouterRequest {
                    context,
                    router_request,
                } = req;
                let (parts, body) = router_request.into_parts();

                // Only show the "receive_body" span if we haven't received the body yet, to prevent
                // having multiple of those spans
                let is_fixed_size = body.size_hint().exact().is_some();
                let bytes = if is_fixed_size {
                    router::body::into_bytes(body).await?
                } else {
                    router::body::into_bytes(body)
                        .instrument(tracing::debug_span!("receive_body"))
                        .await?
                };

                let mut attrs = Vec::with_capacity(5);
                #[cfg(test)]
                let mut headers: indexmap::IndexMap<String, http::HeaderValue> = parts
                    .headers
                    .clone()
                    .into_iter()
                    .filter_map(|(name, val)| Some((name?.to_string(), val)))
                    .collect();
                #[cfg(test)]
                headers.sort_keys();
                #[cfg(not(test))]
                let headers = &parts.headers;

                attrs.push(KeyValue::new(
                    HTTP_REQUEST_HEADERS,
                    opentelemetry::Value::String(format!("{:?}", headers).into()),
                ));
                attrs.push(KeyValue::new(
                    HTTP_REQUEST_METHOD,
                    opentelemetry::Value::String(format!("{}", parts.method).into()),
                ));
                attrs.push(KeyValue::new(
                    HTTP_REQUEST_URI,
                    opentelemetry::Value::String(format!("{}", parts.uri).into()),
                ));
                attrs.push(KeyValue::new(
                    HTTP_REQUEST_VERSION,
                    opentelemetry::Value::String(format!("{:?}", parts.version).into()),
                ));
                attrs.push(KeyValue::new(
                    HTTP_REQUEST_BODY,
                    opentelemetry::Value::String(
                        format!("{:?}", String::from_utf8_lossy(&bytes)).into(),
                    ),
                ));
                log_event(level, "router.request", attrs, "");

                let body = router::body::from_bytes(bytes);
                req = RouterRequest {
                    context,
                    router_request: http::Request::from_parts(parts, body),
                };
            }

            inner.call(req).await
        })
    }
}

/// A layer that translates router requests (streaming http bodies) into supergraph requests
/// (JSON bodies in the GraphQL spec format).
struct RouterToSupergraphRequestLayer;

impl<S> tower::Layer<S> for RouterToSupergraphRequestLayer {
    type Service = RouterToSupergraphRequestService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RouterToSupergraphRequestService {
            supergraph_service: inner,
        }
    }
}

/// A service that translates router requests (streaming http bodies) into supergraph requests
/// (JSON bodies in the GraphQL spec format).
#[derive(Clone)]
struct RouterToSupergraphRequestService<S> {
    supergraph_service: S, // <supergraph::BoxCloneService>,
}

impl<S> Service<RouterRequest> for RouterToSupergraphRequestService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.supergraph_service.poll_ready(cx)
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        let self_clone = self.clone();
        let mut this = std::mem::replace(self, self_clone);

        Box::pin(async move { this.call_inner(req).await })
    }
}

impl<S> RouterToSupergraphRequestService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError> + Send + 'static,
    S::Future: Send + 'static,
{
    async fn call_inner(&mut self, req: RouterRequest) -> Result<RouterResponse, BoxError> {
        let context = req.context;
        let (parts, body) = req.router_request.into_parts();
        let request = Self::get_graphql_request(&parts, body)
            .await?
            .and_then(|r| Self::translate_request(&context, parts, r));

        let supergraph_request = match request {
            Ok(request) => request,
            Err(err) => {
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

        let SupergraphResponse { context, response } =
            self.supergraph_service.call(supergraph_request).await?;

        // XXX(@goto-bus-stop): *all* of the code using these `accepts_` variables looks like it
        // duplicates what the content_negotiation::SupergraphLayer is doing. We should delete one
        // or the other, and absolutely not do it inline here.
        let ClientRequestAccepts {
            wildcard: accepts_wildcard,
            json: accepts_json,
            multipart_defer: accepts_multipart_defer,
            multipart_subscription: accepts_multipart_subscription,
        } = context
            .extensions()
            .with_lock(|lock| lock.get().cloned())
            .unwrap_or_default();

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
                        .body(router::body::from_bytes(
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
                    let errors = response.errors.clone();

                    parts
                        .headers
                        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
                    let body: Result<String, BoxError> = tracing::trace_span!("serialize_response")
                        .in_scope(|| {
                            let body = serde_json::to_string(&response)?;
                            Ok(body)
                        });
                    let body = body?;
                    // XXX(@goto-bus-stop): I strongly suspect that it would be better to move this into its own layer.
                    let display_router_response = context
                        .extensions()
                        .with_lock(|ext| ext.get::<DisplayRouterResponse>().is_some());

                    router::Response::http_response_builder()
                        .response(Response::from_parts(
                            parts,
                            router::body::from_bytes(body.clone()),
                        ))
                        .and_body_to_stash(if display_router_response {
                            Some(body)
                        } else {
                            None
                        })
                        .context(context)
                        .errors_for_context(errors)
                        .build()
                } else if accepts_multipart_defer || accepts_multipart_subscription {
                    let errors = response.errors.clone();

                    if accepts_multipart_defer {
                        parts.headers.insert(
                            CONTENT_TYPE,
                            MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE.clone(),
                        );
                    } else if accepts_multipart_subscription {
                        parts.headers.insert(
                            CONTENT_TYPE,
                            MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE.clone(),
                        );
                    }
                    // Useful when you're using a proxy like nginx which enable proxy_buffering by default (http://nginx.org/en/docs/http/ngx_http_proxy_module.html#proxy_buffering)
                    parts.headers.insert(
                        ACCEL_BUFFERING_HEADER_NAME.clone(),
                        ACCEL_BUFFERING_HEADER_VALUE.clone(),
                    );

                    let response = match response.subscribed {
                        Some(true) => http::Response::from_parts(
                            parts,
                            router::body::from_result_stream(Multipart::new(
                                body,
                                ProtocolMode::Subscription,
                            )),
                        ),
                        _ => http::Response::from_parts(
                            parts,
                            router::body::from_result_stream(Multipart::new(
                                once(ready(response)).chain(body),
                                ProtocolMode::Defer,
                            )),
                        ),
                    };

                    router::Response::http_response_builder()
                        .response(response)
                        .context(context)
                        .errors_for_context(errors)
                        .build()
                } else {
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

    fn translate_query_request(parts: &Parts) -> Result<graphql::Request, TranslateError> {
        parts.uri.query().map(|q| {
            match graphql::Request::from_urlencoded_query(q.to_string()) {
                Ok(request) => Ok(request),
                Err(err) => {
                    Err(TranslateError {
                        status: StatusCode::BAD_REQUEST,
                        extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                        extension_details: format!(
                            "failed to decode a valid GraphQL request from path {err}"
                        ),
                    })
                }
            }
        }).unwrap_or_else(|| {
            Err(TranslateError {
                status: StatusCode::BAD_REQUEST,
                extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                extension_details: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.".to_string()
            })
        })
    }

    fn translate_bytes_request(bytes: &Bytes) -> Result<graphql::Request, TranslateError> {
        graphql::Request::deserialize_from_bytes(bytes).map_err(|err| TranslateError {
            status: StatusCode::BAD_REQUEST,
            extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
            extension_details: format!("failed to deserialize the request body into JSON: {err}"),
        })
    }

    // Translate parsed JSON GraphQL requests into supergraph requests.
    fn translate_request(
        context: &Context,
        parts: Parts,
        graphql_request: graphql::Request,
    ) -> Result<SupergraphRequest, TranslateError> {
        Ok(SupergraphRequest {
            context: context.clone(),
            supergraph_request: http::Request::from_parts(parts, graphql_request),
        })
    }

    async fn get_graphql_request(
        parts: &Parts,
        body: Body,
    ) -> Result<Result<graphql::Request, TranslateError>, BoxError> {
        let graphql_request = if parts.method == Method::GET {
            Self::translate_query_request(parts)
        } else {
            // Only show the "receive_body" span if we haven't received the body yet, to prevent
            // having multiple of those spans
            let is_fixed_size = body.size_hint().exact().is_some();
            let bytes = if is_fixed_size {
                router::body::into_bytes(body).await?
            } else {
                router::body::into_bytes(body)
                    .instrument(tracing::debug_span!("receive_body"))
                    .await?
            };

            Self::translate_bytes_request(&bytes)
        };
        Ok(graphql_request)
    }
}

#[derive(Clone)]
struct TranslateError {
    status: StatusCode,
    extension_code: String,
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
    sb: UnconstrainedBuffer<router::Request, BoxFuture<'static, router::ServiceResult>>,
    pipeline_handle: Arc<PipelineHandle>,
    /// The configuration used to create this router, stored for hot reload previous config extraction
    pub(crate) configuration: Arc<Configuration>,
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

    fn pipeline_ref(&self) -> Arc<PipelineRef> {
        self.pipeline_handle.pipeline_ref.clone()
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
        // There is a problem here.
        // APQ isn't a plugin and so cannot participate in plugin lifecycle events.
        // After telemetry `activate` NO part of the pipeline can fail as globals have been interacted with.
        // However, the APQLayer uses DeduplicatingCache which is fallible. So if this fails on hot reload the router will be
        // left in an inconsistent state and all metrics will likely stop working.
        // Fixing this will require a larger refactor to bring APQ into the router lifecycle.
        // For now just call activate to make the gauges work on the happy path.
        apq_layer.activate();

        // Create a handle that will help us keep track of this pipeline.
        // A metric is exposed that allows the use to see if pipelines are being hung onto.
        let schema_id = supergraph_creator.schema().schema_id.to_string();
        let launch_id = supergraph_creator
            .schema()
            .launch_id
            .as_ref()
            .map(|launch_id| launch_id.to_string());
        let config_hash = configuration.hash();
        let pipeline_handle = PipelineHandle::new(schema_id, launch_id, config_hash);

        let router_service = content_negotiation::RouterLayer::default().layer(RouterService::new(
            supergraph_creator.create(),
            apq_layer,
            persisted_query_layer,
            query_analysis_layer,
            configuration.batching.clone(),
        ));

        // NOTE: This is the start of the router pipeline (router_service)
        let sb = UnconstrainedBuffer::new(
            ServiceBuilder::new()
                .layer(static_page.clone())
                .service(
                    supergraph_creator
                        .plugins()
                        .iter()
                        .rev()
                        .fold(router_service.boxed(), |acc, (_, e)| e.router_service(acc)),
                )
                .boxed(),
            DEFAULT_BUFFER_SIZE,
        );

        Ok(Self {
            supergraph_creator,
            sb,
            pipeline_handle: Arc::new(pipeline_handle),
            configuration,
        })
    }

    pub(crate) fn make(
        &self,
    ) -> impl Service<
        router::Request,
        Response = router::Response,
        Error = BoxError,
        Future = BoxFuture<'static, router::ServiceResult>,
    > + Send
    + use<> {
        // Note: We have to box our cloned service to erase the type of the Buffer.
        self.sb.clone().boxed()
    }
}

impl RouterCreator {
    pub(crate) fn previous_cache(&self) -> InMemoryCachePlanner {
        self.supergraph_creator.previous_cache()
    }
}
