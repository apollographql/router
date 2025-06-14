use std::sync::Arc;
use std::task::Poll;

use bytes::BufMut;
use bytes::Bytes;
use bytes::BytesMut;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use futures::future::join_all;
use http::Method;
use http::StatusCode;
use http::header;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt as _;
use tracing::Instrument as _;

// FIXME(@goto-bus-stop): Ideally the batching layer shouldn't have to care about this
use crate::Context;
use crate::batching::Batch;
use crate::batching::BatchQuery;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::graphql;
use crate::plugins::content_negotiation::ClientRequestAccepts;
use crate::services::router;
use crate::services::router::Request as RouterRequest;
use crate::services::router::Response as RouterResponse;

// FIXME(@goto-bus-stop): This is a copy of router::service::TranslateError.
// This should use open core error derive instead
#[derive(Clone)]
struct TranslateError {
    status: StatusCode,
    extension_code: String,
    extension_details: String,
}

/// When the batching layer receives a batch query (a POST request with a JSON array in the body),
/// it splits the requests into multiple requests that flow separately through the rest of the
/// pipeline, and reassembles the responses into a single JSON array response.
pub(super) struct BatchingLayer {
    config: Batching,
}

impl BatchingLayer {
    pub(super) fn new(config: Batching) -> Self {
        Self { config }
    }
}

impl<S> tower::Layer<S> for BatchingLayer {
    type Service = BatchingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BatchingService {
            inner,
            config: self.config.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct BatchingService<S> {
    inner: S,
    config: Batching,
}
impl<S> Service<RouterRequest> for BatchingService<S>
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

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        // Batching is not supported for GET requests
        if req.router_request.method() == Method::GET {
            return self.inner.call(req).boxed();
        }

        let service = self.clone();
        let mut service = std::mem::replace(self, service);

        Box::pin(async move {
            let context = req.context;
            let (parts, body) = req.router_request.into_parts();

            // REVIEW NOTE(@goto-bus-stop): equivalent of get_graphql_requests

            // FIXME(@goto-bus-stop): This should be the responsibility of the open core HttpToBytesLayer.
            let bytes = router::body::into_bytes(body)
                .instrument(tracing::debug_span!("receive_body"))
                .await?;

            // FIXME(@goto-bus-stop): telemetry from old `get_graphql_requests` should happen in
            // between HttpToBytesLayer and this (or before?)

            let batch = match service.parse_batch_request(&bytes) {
                Ok(None) => {
                    // Not a batch request. Reassemble the request and pass it on.
                    // FIXME(@goto-bus-stop): We won't need this conversion when we have the
                    // open core HttpToBytesLayer
                    let body = router::body::from_bytes(bytes);
                    return service
                        .inner
                        .call(RouterRequest {
                            context,
                            router_request: http::Request::from_parts(parts, body),
                        })
                        .await;
                }
                Ok(Some(batch)) => batch,
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
                        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.essence_str())
                        .context(context)
                        .build();
                }
            };

            // REVIEW NOTE(@goto-bus-stop): the batching-related parts of `translate_request`

            let requests = match service.build_batch_requests(&context, parts, batch) {
                Ok(results) => results,
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
                        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.essence_str())
                        .context(context)
                        .build();
                }
            };

            // We need to handle cases where a failure is part of a batch and thus must be cancelled.
            // Requests can be cancelled at any point of the router pipeline, but all failures bubble back
            // up through here, so we can catch them without having to specially handle batch queries in
            // other portions of the codebase.
            let futures = requests.into_iter().map(|request| {
                let service = service.clone();
                async move {
                    // We clone the context here, because if the request results in an Err, the
                    // response context will no longer exist.
                    let context = request.context.clone();
                    let result = service.call_inner_service(request).await;

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
            let results: Vec<router::Response> = join_all(futures)
                .await
                .into_iter()
                .collect::<Result<Vec<router::Response>, BoxError>>()?;

            // If we detected we are processing a batch, return an array of results even if there is only
            // one result
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

            Ok(RouterResponse {
                response: http::Response::from_parts(
                    parts,
                    router::body::from_bytes(bytes.freeze()),
                ),
                context,
            })
        })
    }
}

impl<S> BatchingService<S>
where
    S: Service<RouterRequest, Response = RouterResponse, Error = BoxError> + Clone,
{
    fn parse_batch_request(
        &self,
        bytes: &Bytes,
    ) -> Result<Option<Vec<graphql::Request>>, TranslateError> {
        // REVIEW NOTE(@goto-bus-stop): Previously, batching first attempted to parse a single
        // response, and only attempted to parse a batch if that failed. With batching as a
        // separate layer, we can't do that anymore (as parsing is the responsibility of a
        // downstream service), so to avoid re-parsing in the unbatched case we need an up front
        // check for if it's likely to be a batch.
        let first_non_ws_character = bytes.iter().find(|byte| !byte.is_ascii_whitespace());
        if first_non_ws_character != Some(&b'[') {
            // Not a batch request
            return Ok(None);
        }

        if self.config.enabled && matches!(self.config.mode, BatchingMode::BatchHttpLink) {
            let result = graphql::Request::batch_from_bytes(bytes).map_err(|e| TranslateError {
                status: StatusCode::BAD_REQUEST,
                extension_code: "INVALID_GRAPHQL_REQUEST".to_string(),
                extension_details: format!("failed to deserialize the request body into JSON: {e}"),
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

            if self.config.exceeds_batch_size(&result) {
                return Err(TranslateError {
                    status: StatusCode::UNPROCESSABLE_ENTITY,
                    extension_code: "BATCH_LIMIT_EXCEEDED".to_string(),
                    extension_details: format!(
                        "Batch limits exceeded: you provided a batch with {} entries, but the configured maximum router batch size is {}",
                        result.len(),
                        self.config.maximum_size.unwrap_or_default()
                    ),
                });
            }

            Ok(Some(result))
        } else {
            let extension_details = if self.config.enabled
                && !matches!(self.config.mode, BatchingMode::BatchHttpLink)
            {
                format!("batching not supported for mode `{}`", self.config.mode)
            } else {
                "batching not enabled".to_string()
            };
            Err(TranslateError {
                status: StatusCode::BAD_REQUEST,
                extension_code: "BATCHING_NOT_ENABLED".to_string(),
                extension_details,
            })
        }
    }

    /// Turns a parsed batch of queries into multiple separate requests that can flow through the
    /// rest of the pipeline.
    fn build_batch_requests(
        &self,
        context: &Context,
        parts: http::request::Parts,
        batch: Vec<graphql::Request>,
    ) -> Result<Vec<RouterRequest>, TranslateError> {
        // REVIEW NOTE(@goto-bus-stop): This is 1:1 the contents of the old `translate_request`
        // method, except it builds `RouterRequest`s instead of `SupergraphRequest`s.

        let mut results = Vec::with_capacity(batch.len());
        let batch_size = batch.len();

        // Modifying our Context extensions.
        // If we are processing a batch (is_batch == true), insert our batching configuration.
        // If subgraph batching configuration exists and is enabled for any of our subgraphs, we create our shared batch details
        context
            .extensions()
            .with_lock(|lock| lock.insert(self.config.clone()));

        let shared_batch_details = self
            .config
            .subgraph
            .as_ref()
            .map(|subgraph_batching_config| {
                subgraph_batching_config.all.enabled
                    || subgraph_batching_config
                        .subgraphs
                        .values()
                        .any(|v| v.enabled)
            })
            .and_then(|a| a.then_some(Arc::new(Batch::spawn_handler(batch_size))));

        let mut ok_results_it = batch.into_iter();
        let first = ok_results_it
            .next()
            .expect("we should have at least one request");

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
            // FIXME(@goto-bus-stop): we won't need to turn this into a Body when we have the
            // open core HttpToBytesLayer
            let body = router::body::from_bytes(serde_json::to_vec(&graphql_request).unwrap());
            let new = http::Request::from_parts(parts.clone(), body);
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
                lock.insert(self.config.clone());
                // We are only going to insert a BatchQuery if Subgraph processing is enabled
                if let Some(b_for_index) = b_for_index_opt {
                    lock.insert(b_for_index);
                }
            });
            results.push(RouterRequest {
                router_request: new,
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

        let body = router::body::from_bytes(serde_json::to_vec(&first).unwrap());
        results.insert(
            0,
            RouterRequest {
                router_request: http::Request::from_parts(parts, body),
                context: context.clone(),
            },
        );

        Ok(results)
    }

    async fn call_inner_service(self, request: RouterRequest) -> Result<RouterResponse, BoxError> {
        // self.inner here is a clone of the service that was readied
        // in RouterService::poll_ready. Clones are unready by default, so this
        // self.inner is actually not ready, which is why we need to
        // oneshot it here. That technically breaks backpressure, but because we are
        // still readying the supergraph service before calling into the router
        // service, backpressure is actually still exerted at that point--there's
        // just potential for some requests to slip through the cracks and end up
        // queueing up at this .oneshot() call.
        //
        // Not ideal, but an improvement on the situation in Router 1.x.
        self.inner.oneshot(request).await
    }
}
