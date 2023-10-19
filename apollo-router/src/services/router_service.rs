//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use axum::body::StreamBody;
use axum::response::*;
use bytes::BufMut;
use bytes::Bytes;
use bytes::BytesMut;
use futures::future::join_all;
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
use super::layers::content_negotiation;
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
use super::APPLICATION_JSON_HEADER_VALUE;
use super::MULTIPART_DEFER_CONTENT_TYPE;
use super::MULTIPART_SUBSCRIPTION_CONTENT_TYPE;
use crate::cache::DeduplicatingCache;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::graphql;
use crate::http_ext;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::protocols::multipart::Multipart;
use crate::protocols::multipart::ProtocolMode;
use crate::query_planner::QueryPlanResult;
use crate::query_planner::WarmUpCachingQueryKey;
use crate::router_factory::RouterFactory;
use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::Configuration;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;

pub(crate) static MULTIPART_DEFER_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE);
pub(crate) static MULTIPART_SUBSCRIPTION_HEADER_VALUE: HeaderValue =
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
    experimental_http_max_request_bytes: usize,
    experimental_batching: Batching,
}

impl RouterService {
    pub(crate) fn new(
        supergraph_creator: Arc<SupergraphCreator>,
        apq_layer: APQLayer,
        persisted_query_layer: Arc<PersistedQueryLayer>,
        query_analysis_layer: QueryAnalysisLayer,
        experimental_http_max_request_bytes: usize,
        experimental_batching: Batching,
    ) -> Self {
        RouterService {
            supergraph_creator,
            apq_layer,
            persisted_query_layer,
            query_analysis_layer,
            experimental_http_max_request_bytes,
            experimental_batching,
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
    async fn process_supergraph_request(
        &self,
        supergraph_request: SupergraphRequest,
    ) -> Result<router::Response, BoxError> {
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
                    Ok(request) => self.supergraph_creator.create().oneshot(request).await?,
                },
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
                    parts
                        .headers
                        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
                    tracing::trace_span!("serialize_response").in_scope(|| {
                        let body = serde_json::to_string(&response)?;
                        Ok(router::Response {
                            response: http::Response::from_parts(parts, Body::from(body)),
                            context,
                        })
                    })
                } else if accepts_multipart_defer || accepts_multipart_subscription {
                    if accepts_multipart_defer {
                        parts
                            .headers
                            .insert(CONTENT_TYPE, MULTIPART_DEFER_HEADER_VALUE.clone());
                    } else if accepts_multipart_subscription {
                        parts
                            .headers
                            .insert(CONTENT_TYPE, MULTIPART_SUBSCRIPTION_HEADER_VALUE.clone());
                    }
                    // Useful when you're using a proxy like nginx which enable proxy_buffering by default (http://nginx.org/en/docs/http/ngx_http_proxy_module.html#proxy_buffering)
                    parts.headers.insert(
                        ACCEL_BUFFERING_HEADER_NAME.clone(),
                        ACCEL_BUFFERING_HEADER_VALUE.clone(),
                    );
                    let multipart_stream = match response.subscribed {
                        Some(true) => {
                            StreamBody::new(Multipart::new(body, ProtocolMode::Subscription))
                        }
                        _ => StreamBody::new(Multipart::new(
                            once(ready(response)).chain(body),
                            ProtocolMode::Defer,
                        )),
                    };
                    let response = (parts, multipart_stream).into_response().map(|body| {
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
                    });

                    Ok(RouterResponse { response, context })
                } else {
                    // this should be unreachable due to a previous check, but just to be sure...
                    Ok(router::Response::error_builder()
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
                            .build()?)
                }
            }
        }
    }

    async fn call_inner(&self, req: RouterRequest) -> Result<RouterResponse, BoxError> {
        let context = req.context.clone();

        let supergraph_requests = match self.translate_request(req).await {
            Ok(requests) => requests,
            Err(err) => {
                ::tracing::error!(
                    monotonic_counter.apollo_router_http_requests_total = 1u64,
                    status = %err.status.as_u16(),
                    error = %err.error,
                );

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

        let futures = supergraph_requests
            .into_iter()
            .map(|supergraph_request| self.process_supergraph_request(supergraph_request));

        // Use join_all to preserve ordering of concurrent operations
        // (Short circuit processing and propagate any errors in the batch)
        let mut results: Vec<router::Response> = join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<router::Response>, BoxError>>()?;

        // If we only have one result, go ahead and return it. Otherwise, create a new result
        // which is an array of all results.
        if results.len() == 1 {
            Ok(results.pop().expect("we should have at least one response"))
        } else {
            let first = results.pop().expect("we should have at least one response");
            let (parts, body) = first.response.into_parts();
            let context = first.context;
            let mut bytes = BytesMut::new();
            bytes.put_u8(b'[');
            bytes.extend_from_slice(&hyper::body::to_bytes(body).await?);
            for result in results {
                bytes.put(&b", "[..]);
                bytes.extend_from_slice(&hyper::body::to_bytes(result.response.into_body()).await?);
            }
            bytes.put_u8(b']');

            Ok(RouterResponse {
                response: http::Response::from_parts(parts, Body::from(bytes.freeze())),
                context,
            })
        }
    }

    async fn translate_query_request(
        &self,
        parts: &Parts,
    ) -> Result<Vec<graphql::Request>, TranslateError> {
        parts.uri.query().map(|q| {
            let mut result = vec![];

            match graphql::Request::from_urlencoded_query(q.to_string()) {
                Ok(request) => {
                    result.push(request);
                }
                Err(err) => {
                    // It may be a batch of requests, so try that (if config allows) before
                    // erroring out
                    if self.experimental_batching.enabled
                        && matches!(self.experimental_batching.mode, BatchingMode::BatchHttpLink)
                    {
                        result = graphql::Request::batch_from_urlencoded_query(q.to_string())
                            .map_err(|e| TranslateError {
                                status: StatusCode::BAD_REQUEST,
                                error: "failed to decode a valid GraphQL request from path",
                                extension_code: "INVALID_GRAPHQL_REQUEST",
                                extension_details: format!(
                                    "failed to decode a valid GraphQL request from path {e}"
                                ),
                            })?;
                    } else if !q.is_empty() && q.as_bytes()[0] == b'[' {
                        let extension_details = if self.experimental_batching.enabled
                            && !matches!(self.experimental_batching.mode, BatchingMode::BatchHttpLink) {
                            format!("batching not supported for mode `{}`", self.experimental_batching.mode)
                        } else {
                            "batching not enabled".to_string()
                        };
                        return Err(TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            error: "batching not enabled",
                            extension_code: "BATCHING_NOT_ENABLED",
                            extension_details,
                        });
                    } else {
                        return Err(TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            error: "failed to decode a valid GraphQL request from path",
                            extension_code: "INVALID_GRAPHQL_REQUEST",
                            extension_details: format!(
                                "failed to decode a valid GraphQL request from path {err}"
                            ),
                        });
                    }
                }
            };
            Ok(result)
        }).unwrap_or_else(|| {
            Err(TranslateError {
                status: StatusCode::BAD_REQUEST,
                error: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.",
                extension_code: "INVALID_GRAPHQL_REQUEST",
                extension_details: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.".to_string()
            })
        })
    }

    fn translate_bytes_request(
        &self,
        bytes: &Bytes,
    ) -> Result<Vec<graphql::Request>, TranslateError> {
        let mut result = vec![];

        match graphql::Request::deserialize_from_bytes(bytes) {
            Ok(request) => {
                result.push(request);
            }
            Err(err) => {
                if self.experimental_batching.enabled
                    && matches!(self.experimental_batching.mode, BatchingMode::BatchHttpLink)
                {
                    result =
                        graphql::Request::batch_from_bytes(bytes).map_err(|e| TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            error: "failed to deserialize the request body into JSON",
                            extension_code: "INVALID_GRAPHQL_REQUEST",
                            extension_details: format!(
                                "failed to deserialize the request body into JSON: {e}"
                            ),
                        })?;
                } else if !bytes.is_empty() && bytes[0] == b'[' {
                    let extension_details = if self.experimental_batching.enabled
                        && !matches!(self.experimental_batching.mode, BatchingMode::BatchHttpLink)
                    {
                        format!(
                            "batching not supported for mode `{}`",
                            self.experimental_batching.mode
                        )
                    } else {
                        "batching not enabled".to_string()
                    };
                    return Err(TranslateError {
                        status: StatusCode::BAD_REQUEST,
                        error: "batching not enabled",
                        extension_code: "BATCHING_NOT_ENABLED",
                        extension_details,
                    });
                } else {
                    return Err(TranslateError {
                        status: StatusCode::BAD_REQUEST,
                        error: "failed to deserialize the request body into JSON",
                        extension_code: "INVALID_GRAPHQL_REQUEST",
                        extension_details: format!(
                            "failed to deserialize the request body into JSON: {err}"
                        ),
                    });
                }
            }
        };
        Ok(result)
    }

    async fn translate_request(
        &self,
        req: RouterRequest,
    ) -> Result<Vec<SupergraphRequest>, TranslateError> {
        let RouterRequest {
            router_request,
            context,
        } = req;

        let (parts, body) = router_request.into_parts();

        let graphql_requests: Result<Vec<graphql::Request>, TranslateError> = if parts.method
            == Method::GET
        {
            self.translate_query_request(&parts).await
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
            if content_length.unwrap_or(0) > self.experimental_http_max_request_bytes {
                Err(TranslateError {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    error: "payload too large for the `experimental_http_max_request_bytes` configuration",
                    extension_code: "INVALID_GRAPHQL_REQUEST",
                    extension_details: "payload too large".to_string(),
                })
            } else {
                let body = http_body::Limited::new(body, self.experimental_http_max_request_bytes);
                hyper::body::to_bytes(body)
                    .instrument(tracing::debug_span!("receive_body"))
                    .await
                    .map_err(|e| {
                        if e.is::<http_body::LengthLimitError>() {
                            TranslateError {
                                status: StatusCode::PAYLOAD_TOO_LARGE,
                                error: "payload too large for the `experimental_http_max_request_bytes` configuration",
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
                    })
                    .and_then(|bytes| {
                        self.translate_bytes_request(&bytes)
                    })
            }
        };

        let mut ok_results = graphql_requests?;
        let mut results = Vec::with_capacity(ok_results.len());
        let first = ok_results
            .pop()
            .expect("We must have at least one response");
        let sg = http::Request::from_parts(parts, first);

        if !ok_results.is_empty() {
            context
                .private_entries
                .lock()
                .insert(self.experimental_batching.clone());
        }
        // Building up the batch of supergraph requests is tricky.
        // Firstly note that any http extensions are only propagated for the first request sent
        // through the pipeline. This is because there is simply no way to clone http
        // extensions.
        //
        // Secondly, we can't clone private_entries, but we need to propagate at least
        // ClientRequestAccepts to ensure correct processing of the response. We do that manually,
        // but the concern is that there may be other private_entries that wish to propagate into
        // each request or we may add them in future and not know about it here...
        //
        // (Technically we could clone private entries, since it is held under an `Arc`, but that
        // would mean all the requests in a batch shared the same set of private entries and review
        // comments expressed the sentiment that this may be a bad thing...)
        //
        for graphql_request in ok_results {
            // XXX Lose http extensions, is that ok?
            let mut new = http_ext::clone_http_request(&sg);
            *new.body_mut() = graphql_request;
            // XXX Lose some private entries, is that ok?
            let new_context = Context::new();
            new_context.extend(&context);
            let client_request_accepts_opt = context
                .private_entries
                .lock()
                .get::<ClientRequestAccepts>()
                .cloned();
            if let Some(client_request_accepts) = client_request_accepts_opt {
                new_context
                    .private_entries
                    .lock()
                    .insert(client_request_accepts);
            }
            new_context
                .private_entries
                .lock()
                .insert(self.experimental_batching.clone());
            results.push(SupergraphRequest {
                supergraph_request: new,
                // Build a new context. Cloning would cause issues.
                context: new_context,
            });
        }
        results.insert(
            0,
            SupergraphRequest {
                supergraph_request: sg,
                context,
            },
        );
        Ok(results)
    }
}

struct TranslateError<'a> {
    status: StatusCode,
    error: &'a str,
    extension_code: &'a str,
    extension_details: String,
}

// Process the headers to make sure that `VARY` is set correctly
fn process_vary_header(headers: &mut HeaderMap<HeaderValue>) {
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
    experimental_http_max_request_bytes: usize,
    experimental_batching: Batching,
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
                    .await,
            )
        } else {
            APQLayer::disabled()
        };

        Ok(Self {
            supergraph_creator,
            static_page,
            apq_layer,
            query_analysis_layer,
            experimental_http_max_request_bytes: configuration
                .limits
                .experimental_http_max_request_bytes,
            persisted_query_layer,
            experimental_batching: configuration.experimental_batching.clone(),
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
            self.experimental_http_max_request_bytes,
            self.experimental_batching.clone(),
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
    pub(crate) async fn cache_keys(&self, count: Option<usize>) -> Vec<WarmUpCachingQueryKey> {
        self.supergraph_creator.cache_keys(count).await
    }

    pub(crate) fn planner(&self) -> Arc<Planner<QueryPlanResult>> {
        self.supergraph_creator.planner()
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

    #[tokio::test]
    async fn test_experimental_http_max_request_bytes() {
        /// Size of the JSON serialization of the request created by `fn canned_new`
        /// in `apollo-router/src/services/supergraph.rs`
        const CANNED_REQUEST_LEN: usize = 391;

        async fn with_config(experimental_http_max_request_bytes: usize) -> router::Response {
            let http_request = supergraph::Request::canned_builder()
                .build()
                .unwrap()
                .supergraph_request
                .map(|body| {
                    let json_bytes = serde_json::to_vec(&body).unwrap();
                    assert_eq!(
                        json_bytes.len(),
                        CANNED_REQUEST_LEN,
                        "The request generated by `fn canned_new` \
                         in `apollo-router/src/services/supergraph.rs` has changed. \
                         Please update `CANNED_REQUEST_LEN` accordingly."
                    );
                    hyper::Body::from(json_bytes)
                });
            let config = serde_json::json!({
                "limits": {
                    "experimental_http_max_request_bytes": experimental_http_max_request_bytes
                }
            });
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request just at (under) the limit
        let response = with_config(CANNED_REQUEST_LEN).await.response;
        assert_eq!(response.status(), http::StatusCode::OK);

        // Send a request just over the limit
        let response = with_config(CANNED_REQUEST_LEN - 1).await.response;
        assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    }

    //  Test query batching

    #[tokio::test]
    async fn it_only_accepts_batch_http_link_mode_for_query_batch() {
        let expected_response: serde_json::Value = serde_json::from_str(include_str!(
            "query_batching/testdata/batching_not_enabled_response.json"
        ))
        .unwrap();

        async fn with_config() -> router::Response {
            let http_request = supergraph::Request::canned_builder()
                .build()
                .unwrap()
                .supergraph_request
                .map(|req: crate::request::Request| {
                    // Modify the request so that it is a valid array of requests.
                    let mut json_bytes = serde_json::to_vec(&req).unwrap();
                    let mut result = vec![b'['];
                    result.append(&mut json_bytes.clone());
                    result.push(b',');
                    result.append(&mut json_bytes);
                    result.push(b']');
                    hyper::Body::from(result)
                });
            let config = serde_json::json!({});
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request
        let response = with_config().await.response;
        assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
        let data: serde_json::Value =
            serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(expected_response, data);
    }

    #[tokio::test]
    async fn it_processes_a_valid_query_batch() {
        let expected_response: serde_json::Value = serde_json::from_str(include_str!(
            "query_batching/testdata/expected_good_response.json"
        ))
        .unwrap();

        async fn with_config() -> router::Response {
            let http_request = supergraph::Request::canned_builder()
                .build()
                .unwrap()
                .supergraph_request
                .map(|req: crate::request::Request| {
                    // Modify the request so that it is a valid array of requests.
                    let mut json_bytes = serde_json::to_vec(&req).unwrap();
                    let mut result = vec![b'['];
                    result.append(&mut json_bytes.clone());
                    result.push(b',');
                    result.append(&mut json_bytes);
                    result.push(b']');
                    hyper::Body::from(result)
                });
            let config = serde_json::json!({
                "experimental_batching": {
                    "enabled": true,
                    "mode" : "batch_http_link"
                }
            });
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request
        let response = with_config().await.response;
        assert_eq!(response.status(), http::StatusCode::OK);
        let data: serde_json::Value =
            serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(expected_response, data);
    }

    #[tokio::test]
    async fn it_will_not_process_a_query_batch_without_enablement() {
        let expected_response: serde_json::Value = serde_json::from_str(include_str!(
            "query_batching/testdata/batching_not_enabled_response.json"
        ))
        .unwrap();

        async fn with_config() -> router::Response {
            let http_request = supergraph::Request::canned_builder()
                .build()
                .unwrap()
                .supergraph_request
                .map(|req: crate::request::Request| {
                    // Modify the request so that it is a valid array of requests.
                    let mut json_bytes = serde_json::to_vec(&req).unwrap();
                    let mut result = vec![b'['];
                    result.append(&mut json_bytes.clone());
                    result.push(b',');
                    result.append(&mut json_bytes);
                    result.push(b']');
                    hyper::Body::from(result)
                });
            let config = serde_json::json!({});
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request
        let response = with_config().await.response;
        assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
        let data: serde_json::Value =
            serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(expected_response, data);
    }

    #[tokio::test]
    async fn it_will_not_process_a_poorly_formatted_query_batch() {
        let expected_response: serde_json::Value = serde_json::from_str(include_str!(
            "query_batching/testdata/badly_formatted_batch_response.json"
        ))
        .unwrap();

        async fn with_config() -> router::Response {
            let http_request = supergraph::Request::canned_builder()
                .build()
                .unwrap()
                .supergraph_request
                .map(|req: crate::request::Request| {
                    // Modify the request so that it is a valid array of requests.
                    let mut json_bytes = serde_json::to_vec(&req).unwrap();
                    let mut result = vec![b'['];
                    result.append(&mut json_bytes.clone());
                    result.push(b',');
                    result.append(&mut json_bytes);
                    // Deliberately omit the required trailing ]
                    hyper::Body::from(result)
                });
            let config = serde_json::json!({
                "experimental_batching": {
                    "enabled": true,
                    "mode" : "batch_http_link"
                }
            });
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request
        let response = with_config().await.response;
        assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
        let data: serde_json::Value =
            serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(expected_response, data);
    }

    #[tokio::test]
    async fn it_will_process_a_non_batched_defered_query() {
        let expected_response = "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"data\":{\"topProducts\":[{\"upc\":\"1\",\"name\":\"Table\",\"reviews\":[{\"product\":{\"name\":\"Table\"},\"author\":{\"id\":\"1\",\"name\":\"Ada Lovelace\"}},{\"product\":{\"name\":\"Table\"},\"author\":{\"id\":\"2\",\"name\":\"Alan Turing\"}}]},{\"upc\":\"2\",\"name\":\"Couch\",\"reviews\":[{\"product\":{\"name\":\"Couch\"},\"author\":{\"id\":\"1\",\"name\":\"Ada Lovelace\"}}]}]},\"hasNext\":true}\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"hasNext\":false,\"incremental\":[{\"data\":{\"id\":\"1\"},\"path\":[\"topProducts\",0,\"reviews\",0]},{\"data\":{\"id\":\"4\"},\"path\":[\"topProducts\",0,\"reviews\",1]},{\"data\":{\"id\":\"2\"},\"path\":[\"topProducts\",1,\"reviews\",0]}]}\r\n--graphql--\r\n";
        async fn with_config() -> router::Response {
            let query = "
                query TopProducts($first: Int) {
                    topProducts(first: $first) {
                        upc
                        name
                        reviews {
                            ... @defer {
                            id
                            }
                            product { name }
                            author { id name }
                        }
                    }
                }
            ";
            let http_request = supergraph::Request::canned_builder()
                .header(http::header::ACCEPT, MULTIPART_DEFER_CONTENT_TYPE)
                .query(query)
                .build()
                .unwrap()
                .supergraph_request
                .map(|req: crate::request::Request| {
                    let bytes = serde_json::to_vec(&req).unwrap();
                    hyper::Body::from(bytes)
                });
            let config = serde_json::json!({
                "experimental_batching": {
                    "enabled": true,
                    "mode" : "batch_http_link"
                }
            });
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request
        let response = with_config().await.response;
        assert_eq!(response.status(), http::StatusCode::OK);
        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let data = String::from_utf8_lossy(&bytes);
        assert_eq!(expected_response, data);
    }

    #[tokio::test]
    async fn it_will_not_process_a_batched_deferred_query() {
        let expected_response = "[\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"errors\":[{\"message\":\"Deferred responses and subscriptions aren't supported in batches\",\"extensions\":{\"code\":\"BATCHING_DEFER_UNSUPPORTED\"}}]}\r\n--graphql--\r\n, \r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"errors\":[{\"message\":\"Deferred responses and subscriptions aren't supported in batches\",\"extensions\":{\"code\":\"BATCHING_DEFER_UNSUPPORTED\"}}]}\r\n--graphql--\r\n]";

        async fn with_config() -> router::Response {
            let query = "
                query TopProducts($first: Int) {
                    topProducts(first: $first) {
                        upc
                        name
                        reviews {
                            ... @defer {
                            id
                            }
                            product { name }
                            author { id name }
                        }
                    }
                }
            ";
            let http_request = supergraph::Request::canned_builder()
                .header(http::header::ACCEPT, MULTIPART_DEFER_CONTENT_TYPE)
                .query(query)
                .build()
                .unwrap()
                .supergraph_request
                .map(|req: crate::request::Request| {
                    // Modify the request so that it is a valid array of requests.
                    let mut json_bytes = serde_json::to_vec(&req).unwrap();
                    let mut result = vec![b'['];
                    result.append(&mut json_bytes.clone());
                    result.push(b',');
                    result.append(&mut json_bytes);
                    result.push(b']');
                    hyper::Body::from(result)
                });
            let config = serde_json::json!({
                "experimental_batching": {
                    "enabled": true,
                    "mode" : "batch_http_link"
                }
            });
            crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await
                .unwrap()
                .oneshot(router::Request::from(http_request))
                .await
                .unwrap()
        }
        // Send a request
        let response = with_config().await.response;
        assert_eq!(response.status(), http::StatusCode::NOT_ACCEPTABLE);
        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let data = String::from_utf8_lossy(&bytes);
        assert_eq!(expected_response, data);
    }
}
