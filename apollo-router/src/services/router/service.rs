//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use axum::response::*;
use bytes::BufMut;
use bytes::Bytes;
use bytes::BytesMut;
use futures::TryFutureExt;
use futures::future::BoxFuture;
use futures::future::join_all;
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
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_service::Service;
use tracing::Instrument;

use super::Body;
use super::ClientRequestAccepts;
use crate::Configuration;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;
use crate::axum_factory::CanceledRequest;
use crate::batching::Batch;
use crate::batching::BatchQuery;
use crate::cache::DeduplicatingCache;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::graphql;
use crate::http_ext;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::ServiceBuilderExt;
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
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::layers::static_page::StaticPageLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
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
#[derive(Clone)]
pub(crate) struct RouterService {
    apq_layer: Arc<APQLayer>,
    persisted_query_layer: Arc<PersistedQueryLayer>,
    query_analysis_layer: Arc<QueryAnalysisLayer>,
    // Cannot be under Arc. Batching state must be preserved for each RouterService
    // instance
    batching: Batching,
    supergraph_service: supergraph::BoxCloneService,
}

impl RouterService {
    fn new(
        sgb: supergraph::BoxService,
        apq_layer: APQLayer,
        persisted_query_layer: Arc<PersistedQueryLayer>,
        query_analysis_layer: QueryAnalysisLayer,
        batching: Batching,
    ) -> Self {
        let supergraph_service: supergraph::BoxCloneService =
            ServiceBuilder::new().buffered().service(sgb).boxed_clone();

        RouterService {
            apq_layer: Arc::new(apq_layer),
            persisted_query_layer,
            query_analysis_layer: Arc::new(query_analysis_layer),
            batching,
            supergraph_service,
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

impl Service<RouterRequest> for RouterService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.supergraph_service.poll_ready(cx)
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        let self_clone = self.clone();

        let this = std::mem::replace(self, self_clone);

        let fut = async move { this.call_inner(req).await };

        Box::pin(fut)
    }
}

impl RouterService {
    async fn process_supergraph_request(
        self,
        supergraph_request: SupergraphRequest,
    ) -> Result<router::Response, BoxError> {
        // XXX(@goto-bus-stop): This code looks confusing. we are manually calling several
        // layers with various ifs and matches, but *really*, we are calling each layer in order
        // and handling short-circuiting.
        let mut request_res = self
            .persisted_query_layer
            .supergraph_request(supergraph_request);

        if let Ok(supergraph_request) = request_res {
            request_res = self.apq_layer.supergraph_request(supergraph_request).await;
        }

        let SupergraphResponse { response, context } = match request_res {
            Err(response) => response,
            Ok(request) => match self.query_analysis_layer.supergraph_request(request).await {
                Err(response) => response,
                Ok(request) => match self
                    .persisted_query_layer
                    .supergraph_request_with_analyzed_query(request)
                    .await
                {
                    Err(response) => response,
                    Ok(request) => {
                        // self.supergraph_service here is a clone of the service that was readied
                        // in RouterService::poll_ready. Clones are unready by default, so this
                        // self.supergraph_service is actually not ready, which is why we need to
                        // oneshot it here. That technically breaks backpressure, but because we are
                        // still readying the supergraph service before calling into the router
                        // service, backpressure is actually still exerted at that point--there's
                        // just potential for some requests to slip through the cracks and end up
                        // queueing up at this .oneshot() call.
                        //
                        // Not ideal, but an improvement on the situation in Router 1.x.
                        self.supergraph_service.oneshot(request).await?
                    }
                },
            },
        };

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
                router::Response::error_builder()
                    .error(
                        graphql::Error::builder()
                            .message(String::from(
                                "router service is not available to process request",
                            ))
                            .extension_code(StatusCode::SERVICE_UNAVAILABLE.to_string())
                            .build(),
                    )
                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .context(context)
                    .build()
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

                    let errors = response.errors.clone();

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
                    RouterResponse::http_response_builder()
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

    async fn call_inner(self, req: RouterRequest) -> Result<RouterResponse, BoxError> {
        let context = req.context;
        let (parts, body) = req.router_request.into_parts();
        let requests = self
            .clone()
            .get_graphql_requests(&context, &parts, body)
            .await?;

        let my_self = self.clone();
        let (supergraph_requests, is_batch) = match futures::future::ready(requests)
            .and_then(|r| my_self.translate_request(&context, parts, r))
            .await
        {
            Ok(requests) => requests,
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

        // We need to handle cases where a failure is part of a batch and thus must be cancelled.
        // Requests can be cancelled at any point of the router pipeline, but all failures bubble back
        // up through here, so we can catch them without having to specially handle batch queries in
        // other portions of the codebase.
        let futures = supergraph_requests.into_iter().map(|supergraph_request| {
            let my_self = self.clone();
            async move {
                // We clone the context here, because if the request results in an Err, the
                // response context will no longer exist.
                let context = supergraph_request.context.clone();
                let result = my_self.process_supergraph_request(supergraph_request).await;

                // Regardless of the result, we need to make sure that we cancel any potential batch queries. This is because
                // custom rust plugins, rhai scripts, and coprocessors can cancel requests at any time and return a GraphQL
                // error wrapped in an `Ok` or in a `BoxError` wrapped in an `Err`.
                let batch_query_opt = context
                    .extensions()
                    .with_lock(|lock| lock.remove::<BatchQuery>());
                if let Some(batch_query) = batch_query_opt {
                    // Only proceed with signalling cancelled if the batch_query is not finished
                    if !batch_query.finished() {
                        tracing::debug!("cancelling batch query in supergraph response");
                        batch_query
                            .signal_cancelled("request terminated by user".to_string())
                            .await?;
                    }
                }

                result
            }
        });

        // Use join_all to preserve ordering of concurrent operations
        // (Short circuit processing and propagate any errors in the batch)
        // Note: We use `join_all` here since it awaits all futures before returning, thus allowing us to
        // handle cancellation logic without fear of the other futures getting killed.
        let mut results: Vec<router::Response> = join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<router::Response>, BoxError>>()?;

        // If we detected we are processing a batch, return an array of results even if there is only
        // one result
        if is_batch {
            let mut results_it = results.into_iter();
            let first = results_it
                .next()
                .expect("we should have at least one response");
            let (parts, body) = first.response.into_parts();
            let context = first.context;
            let mut bytes = BytesMut::new();
            bytes.put_u8(b'[');
            bytes.extend_from_slice(&router::body::into_bytes(body).await?);
            for result in results_it {
                bytes.put(&b", "[..]);
                bytes.extend_from_slice(
                    &router::body::into_bytes(result.response.into_body()).await?,
                );
            }
            bytes.put_u8(b']');

            RouterResponse::http_response_builder()
                .response(http::Response::from_parts(
                    parts,
                    router::body::from_bytes(bytes.freeze()),
                ))
                .context(context)
                .build()
        } else {
            Ok(results.pop().expect("we should have at least one response"))
        }
    }

    async fn translate_query_request(
        self,
        parts: &Parts,
    ) -> Result<(Vec<graphql::Request>, bool), TranslateError> {
        parts.uri.query().map(|q| {
            match graphql::Request::from_urlencoded_query(q.to_string()) {
                Ok(request) => {
                    Ok((vec![request], false))
                }
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
            // note: this can occur in the legitimate case of a GET request
            // which sends a PQ ID using a custom mechanism processed by a
            // plugin that sets `apollo_persisted_queries::operation_id`. As a
            // workaround, clients can send some dummy query string, or even
            // just include a trailing `?`, but maybe this error is too harsh in
            // that case and we should allow an empty graphql::Request if that
            // context key is set?
            Err(TranslateError {
                status: StatusCode::BAD_REQUEST,
                extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                extension_details: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.".to_string()
            })
        })
    }

    fn translate_bytes_request(
        &self,
        bytes: &Bytes,
    ) -> Result<(Vec<graphql::Request>, bool), TranslateError> {
        let mut result = vec![];
        let mut is_batch = false;

        match graphql::Request::deserialize_from_bytes(bytes) {
            Ok(request) => {
                result.push(request);
            }
            Err(err) => {
                if self.batching.enabled
                    && matches!(self.batching.mode, BatchingMode::BatchHttpLink)
                {
                    result =
                        graphql::Request::batch_from_bytes(bytes).map_err(|e| TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                            extension_details: format!(
                                "failed to deserialize the request body into JSON: {e}"
                            ),
                        })?;
                    if result.is_empty() {
                        return Err(TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                            extension_details:
                                "failed to decode a valid GraphQL request from path: empty array "
                                    .to_string(),
                        });
                    }
                    is_batch = true;
                } else if !bytes.is_empty() && bytes[0] == b'[' {
                    let extension_details = if self.batching.enabled
                        && !matches!(self.batching.mode, BatchingMode::BatchHttpLink)
                    {
                        format!("batching not supported for mode `{}`", self.batching.mode)
                    } else {
                        "batching not enabled".to_string()
                    };
                    return Err(TranslateError {
                        status: StatusCode::BAD_REQUEST,
                        extension_code: "BATCHING_NOT_ENABLED".to_string(),
                        extension_details,
                    });
                } else {
                    return Err(TranslateError {
                        status: StatusCode::BAD_REQUEST,
                        extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                        extension_details: format!(
                            "failed to deserialize the request body into JSON: {err}"
                        ),
                    });
                }
            }
        };

        if is_batch && self.batching.exceeds_batch_size(&result) {
            return Err(TranslateError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                extension_code: "BATCH_LIMIT_EXCEEDED".to_string(),
                extension_details: format!(
                    "Batch limits exceeded: you provided a batch with {} entries, but the configured maximum router batch size is {}",
                    result.len(),
                    self.batching.maximum_size.unwrap_or_default()
                ),
            });
        }

        Ok((result, is_batch))
    }

    async fn translate_request(
        self,
        context: &Context,
        parts: Parts,
        graphql_requests: (Vec<graphql::Request>, bool),
    ) -> Result<(Vec<SupergraphRequest>, bool), TranslateError> {
        let (ok_results, is_batch) = graphql_requests;
        let mut results = Vec::with_capacity(ok_results.len());
        let batch_size = ok_results.len();

        // Modifying our Context extensions.
        // If we are processing a batch (is_batch == true), insert our batching configuration.
        // If subgraph batching configuration exists and is enabled for any of our subgraphs, we create our shared batch details
        let shared_batch_details = (is_batch)
            .then(|| {
                context
                    .extensions()
                    .with_lock(|lock| lock.insert(self.batching.clone()));

                self.batching.subgraph.as_ref()
            })
            .flatten()
            .map(|subgraph_batching_config| {
                subgraph_batching_config.all.enabled
                    || subgraph_batching_config
                        .subgraphs
                        .values()
                        .any(|v| v.enabled)
            })
            .and_then(|a| a.then_some(Arc::new(Batch::spawn_handler(batch_size))));

        let mut ok_results_it = ok_results.into_iter();
        let first = ok_results_it
            .next()
            .expect("we should have at least one request");
        let sg = http::Request::from_parts(parts, first);

        // Building up the batch of supergraph requests is tricky.
        // Firstly note that any http extensions are only propagated for the first request sent
        // through the pipeline. This is because there is simply no way to clone http
        // extensions.
        //
        // Secondly, we can't clone extensions, but we need to propagate at least
        // ClientRequestAccepts to ensure correct processing of the response. We do that manually,
        // but the concern is that there may be other extensions that wish to propagate into
        // each request or we may add them in future and not know about it here...
        //
        // (Technically we could clone extensions, since it is held under an `Arc`, but that
        // would mean all the requests in a batch shared the same set of extensions and review
        // comments expressed the sentiment that this may be a bad thing...)
        //
        // Note: If we enter this loop, then we must be processing a batch.
        for (index, graphql_request) in ok_results_it.enumerate() {
            let mut new = http_ext::clone_http_request(&sg);
            *new.body_mut() = graphql_request;
            // XXX Lose some private entries, is that ok?
            let new_context = Context::new();
            new_context.extend(context);
            let client_request_accepts_opt = context
                .extensions()
                .with_lock(|lock| lock.get::<ClientRequestAccepts>().cloned());
            // We are only going to insert a BatchQuery if Subgraph processing is enabled
            let b_for_index_opt = if let Some(shared_batch_details) = &shared_batch_details {
                Some(
                    Batch::query_for_index(shared_batch_details.clone(), index + 1).map_err(
                        |err| TranslateError {
                            status: StatusCode::INTERNAL_SERVER_ERROR,
                            extension_code: "BATCHING_ERROR".to_string(),
                            extension_details: format!("failed to create batch entry: {err}"),
                        },
                    )?,
                )
            } else {
                None
            };
            new_context.extensions().with_lock(|lock| {
                if let Some(client_request_accepts) = client_request_accepts_opt {
                    lock.insert(client_request_accepts);
                }
                lock.insert(self.batching.clone());
                // We are only going to insert a BatchQuery if Subgraph processing is enabled
                if let Some(b_for_index) = b_for_index_opt {
                    lock.insert(b_for_index);
                }
            });
            results.push(SupergraphRequest {
                supergraph_request: new,
                context: new_context,
            });
        }

        if let Some(shared_batch_details) = shared_batch_details {
            let b_for_index =
                Batch::query_for_index(shared_batch_details, 0).map_err(|err| TranslateError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    extension_code: "BATCHING_ERROR".to_string(),
                    extension_details: format!("failed to create batch entry: {err}"),
                })?;
            context
                .extensions()
                .with_lock(|lock| lock.insert(b_for_index));
        }

        results.insert(
            0,
            SupergraphRequest {
                supergraph_request: sg,
                context: context.clone(),
            },
        );

        Ok((results, is_batch))
    }

    async fn get_graphql_requests(
        self,
        context: &Context,
        parts: &Parts,
        body: Body,
    ) -> Result<Result<(Vec<graphql::Request>, bool), TranslateError>, BoxError> {
        let graphql_requests: Result<(Vec<graphql::Request>, bool), TranslateError> =
            if parts.method == Method::GET {
                self.translate_query_request(parts).await
            } else {
                let bytes = router::body::into_bytes(body)
                    .instrument(tracing::debug_span!("receive_body"))
                    .await?;
                if let Some(level) = context
                    .extensions()
                    .with_lock(|ext| ext.get::<DisplayRouterRequest>().cloned())
                    .map(|d| d.0)
                {
                    let mut attrs = Vec::with_capacity(5);
                    #[cfg(test)]
                    let mut headers: indexmap::IndexMap<String, HeaderValue> = parts
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
                        opentelemetry::Value::String(format!("{headers:?}").into()),
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
                }
                self.translate_bytes_request(&bytes)
            };
        Ok(graphql_requests)
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
    sb: Buffer<router::Request, BoxFuture<'static, router::ServiceResult>>,
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
        let sb = Buffer::new(
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
