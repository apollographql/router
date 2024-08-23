use std::collections::HashMap;
use std::future::ready;
use std::sync::Arc;
use std::task::Poll;

use axum::body;
use axum::response;
use futures::future::join_all;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::StreamExt;
use http::header::CONTENT_TYPE;
use http::request::Parts;
use http::Method;
use http::StatusCode;
use mime::APPLICATION_JSON;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

use super::JsonStream;
use super::Request as JsonRequest;
use super::Response as JsonResponse;
use crate::axum_factory::CanceledRequest;
use crate::batching::Batch;
use crate::batching::BatchQuery;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::graphql;
use crate::http_ext;
use crate::protocols::multipart::SubscriptionPayload;
use crate::services::layers::apq::APQLayer;
use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::router::service::process_vary_header;
use crate::services::router::service::MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE;
use crate::services::router::service::MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE;
use crate::services::router::ClientRequestAccepts;
use crate::services::SupergraphCreator;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::MULTIPART_DEFER_ACCEPT;
use crate::services::MULTIPART_SUBSCRIPTION_ACCEPT;
use crate::Context;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct JsonServerService {
    pub(crate) supergraph_creator: Arc<SupergraphCreator>,
    apq_layer: APQLayer,
    persisted_query_layer: Arc<PersistedQueryLayer>,
    query_analysis_layer: QueryAnalysisLayer,
    batching: Batching,
}

#[buildstructor::buildstructor]
impl JsonServerService {
    #[builder]
    pub(crate) fn new(
        supergraph_creator: Arc<SupergraphCreator>,
        apq_layer: APQLayer,
        persisted_query_layer: Arc<PersistedQueryLayer>,
        query_analysis_layer: QueryAnalysisLayer,
        batching: Batching,
    ) -> Self {
        JsonServerService {
            supergraph_creator,
            apq_layer,
            persisted_query_layer,
            query_analysis_layer,
            batching,
        }
    }
}

impl Service<JsonRequest> for JsonServerService {
    type Response = JsonResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: JsonRequest) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { service.call_inner(request).await })
    }
}

impl JsonServerService {
    async fn call_inner(&self, req: JsonRequest) -> Result<JsonResponse, BoxError> {
        let context = req.context.clone();

        let (supergraph_requests, is_batch) = match self.translate_request(req).await {
            Ok(requests) => requests,
            Err(err) => {
                //FIXME: remove?
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

                return JsonResponse::error_builder()
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
        let futures = supergraph_requests
            .into_iter()
            .map(|supergraph_request| async {
                // We clone the context here, because if the request results in an Err, the
                // response context will no longer exist.
                let context = supergraph_request.context.clone();
                let result = self.process_supergraph_request(supergraph_request).await;

                // Regardless of the result, we need to make sure that we cancel any potential batch queries. This is because
                // custom rust plugins, rhai scripts, and coprocessors can cancel requests at any time and return a GraphQL
                // error wrapped in an `Ok` or in a `BoxError` wrapped in an `Err`.
                let batch_query_opt = context
                    .extensions()
                    .with_lock(|mut lock| lock.remove::<BatchQuery>());
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
            });

        // Use join_all to preserve ordering of concurrent operations
        // (Short circuit processing and propagate any errors in the batch)
        // Note: We use `join_all` here since it awaits all futures before returning, thus allowing us to
        // handle cancellation logic without fear of the other futures getting killed.
        let mut results: Vec<JsonResponse> = join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<JsonResponse>, BoxError>>()?;

        // If we detected we are processing a batch, return an array of results even if there is only
        // one result
        if is_batch {
            let mut results_it = results.into_iter();
            let first = results_it
                .next()
                .expect("we should have at least one response");
            let (parts, body) = first.response.into_parts();
            let mut bodies: Vec<Value> = body.collect().await;
            for response in results_it {
                let (_, mut body) = response.response.into_parts();
                while let Some(v) = body.next().await {
                    bodies.push(v);
                }
            }
            let body = Value::Array(bodies);

            let context = first.context;
            Ok(JsonResponse {
                response: http::Response::from_parts(parts, Box::pin(once(ready(body)))),
                context,
            })
        } else {
            Ok(results.pop().expect("we should have at least one response"))
        }
    }

    async fn translate_query_request(
        &self,
        parts: &Parts,
    ) -> Result<(Vec<graphql::Request>, bool), TranslateError> {
        let mut is_batch = false;
        parts.uri.query().map(|q| {
            let mut result = vec![];

            match graphql::Request::from_urlencoded_query(q.to_string()) {
                Ok(request) => {
                    result.push(request);
                }
                Err(err) => {
                    // It may be a batch of requests, so try that (if config allows) before
                    // erroring out
                    if self.batching.enabled
                        && matches!(self.batching.mode, BatchingMode::BatchHttpLink)
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
                        if result.is_empty() {
                            return Err(TranslateError {
                                status: StatusCode::BAD_REQUEST,
                                error: "failed to decode a valid GraphQL request from path",
                                extension_code: "INVALID_GRAPHQL_REQUEST",
                                extension_details: "failed to decode a valid GraphQL request from path: empty array ".to_string()
                            });
                        }
                        is_batch = true;
                    } else if !q.is_empty() && q.as_bytes()[0] == b'[' {
                        let extension_details = if self.batching.enabled
                            && !matches!(self.batching.mode, BatchingMode::BatchHttpLink) {
                            format!("batching not supported for mode `{}`", self.batching.mode)
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
            Ok((result, is_batch))
        }).unwrap_or_else(|| {
            Err(TranslateError {
                status: StatusCode::BAD_REQUEST,
                error: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.",
                extension_code: "INVALID_GRAPHQL_REQUEST",
                extension_details: "There was no GraphQL operation to execute. Use the `query` parameter to send an operation, using either GET or POST.".to_string()
            })
        })
    }

    fn translate_body_request(
        &self,
        value: Value,
    ) -> Result<(Vec<graphql::Request>, bool), TranslateError> {
        let mut result = vec![];
        let mut is_batch = false;

        match serde_json_bytes::from_value::<graphql::Request>(value.clone()) {
            Ok(request) => {
                result.push(request);
            }
            Err(err) => {
                if self.batching.enabled
                    && matches!(self.batching.mode, BatchingMode::BatchHttpLink)
                {
                    //FIXME: batching works on serde_json::Value, not on serde_json_bytes::Value
                    let v =
                        serde_json_bytes::from_value::<serde_json::Value>(value).map_err(|e| {
                            TranslateError {
                                status: StatusCode::BAD_REQUEST,
                                error: "failed to deserialize the request body into JSON",
                                extension_code: "INVALID_GRAPHQL_REQUEST",
                                extension_details: format!(
                                    "failed to deserialize the request body into JSON: {e}"
                                ),
                            }
                        })?;
                    result =
                        graphql::Request::process_batch_values(&v).map_err(|e| TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            error: "failed to deserialize the request body into JSON",
                            extension_code: "INVALID_GRAPHQL_REQUEST",
                            extension_details: format!(
                                "failed to deserialize the request body into JSON: {e}"
                            ),
                        })?;
                    if result.is_empty() {
                        return Err(TranslateError {
                            status: StatusCode::BAD_REQUEST,
                            error: "failed to decode a valid GraphQL request from path",
                            extension_code: "INVALID_GRAPHQL_REQUEST",
                            extension_details:
                                "failed to decode a valid GraphQL request from path: empty array "
                                    .to_string(),
                        });
                    }
                    is_batch = true;
                } else if value.is_array() {
                    let extension_details = if self.batching.enabled
                        && !matches!(self.batching.mode, BatchingMode::BatchHttpLink)
                    {
                        format!("batching not supported for mode `{}`", self.batching.mode)
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
        Ok((result, is_batch))
    }

    async fn translate_request(
        &self,
        req: JsonRequest,
    ) -> Result<(Vec<SupergraphRequest>, bool), TranslateError> {
        let JsonRequest { request, context } = req;

        let (parts, body) = request.into_parts();

        let graphql_requests: Result<(Vec<graphql::Request>, bool), TranslateError> =
            if parts.method == Method::GET {
                self.translate_query_request(&parts).await
            } else {
                self.translate_body_request(body)
            };

        let (ok_results, is_batch) = graphql_requests?;
        let mut results = Vec::with_capacity(ok_results.len());
        let batch_size = ok_results.len();

        // Modifying our Context extensions.
        // If we are processing a batch (is_batch == true), insert our batching configuration.
        // If subgraph batching configuration exists and is enabled for any of our subgraphs, we create our shared batch details
        let shared_batch_details = (is_batch)
            .then(|| {
                context
                    .extensions()
                    .with_lock(|mut lock| lock.insert(self.batching.clone()));

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
            // XXX Lose http extensions, is that ok?
            let mut new = http_ext::clone_http_request(&sg);
            *new.body_mut() = graphql_request;
            // XXX Lose some private entries, is that ok?
            let new_context = Context::new();
            new_context.extend(&context);
            let client_request_accepts_opt = context
                .extensions()
                .with_lock(|lock| lock.get::<ClientRequestAccepts>().cloned());
            // We are only going to insert a BatchQuery if Subgraph processing is enabled
            let b_for_index_opt = if let Some(shared_batch_details) = &shared_batch_details {
                Some(
                    Batch::query_for_index(shared_batch_details.clone(), index + 1).map_err(
                        |err| TranslateError {
                            status: StatusCode::INTERNAL_SERVER_ERROR,
                            error: "failed to create batch",
                            extension_code: "BATCHING_ERROR",
                            extension_details: format!("failed to create batch entry: {err}"),
                        },
                    )?,
                )
            } else {
                None
            };
            new_context.extensions().with_lock(|mut lock| {
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
                    error: "failed to create batch",
                    extension_code: "BATCHING_ERROR",
                    extension_details: format!("failed to create batch entry: {err}"),
                })?;
            context
                .extensions()
                .with_lock(|mut lock| lock.insert(b_for_index));
        }

        results.insert(
            0,
            SupergraphRequest {
                supergraph_request: sg,
                context,
            },
        );

        Ok((results, is_batch))
    }

    async fn process_supergraph_request(
        &self,
        supergraph_request: SupergraphRequest,
    ) -> Result<JsonResponse, BoxError> {
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
                Ok(JsonResponse::error_builder()
                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                    .error::<graphql::Error>(
                        graphql::Error::builder()
                            .message("router service is not available to process request")
                            .extension_code("SERVICE_UNAVAILABLE")
                            .build(),
                    )
                    .context(context)
                    .build()?)
            }
            Some(mut response) => {
                if !response.has_next.unwrap_or(false)
                    && !response.subscribed.unwrap_or(false)
                    && (accepts_json || accepts_wildcard)
                {
                    if !response.errors.is_empty() {
                        Self::count_errors(&response.errors);
                    }

                    parts
                        .headers
                        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
                    tracing::trace_span!("serialize_response").in_scope(|| {
                        let body = serde_json_bytes::to_value(&response)?;
                        Ok(JsonResponse {
                            response: http::Response::from_parts(
                                parts,
                                Box::pin(once(ready(body))),
                            ),
                            context,
                        })
                    })
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

                    if !response.errors.is_empty() {
                        Self::count_errors(&response.errors);
                    }
                    let response = if response.subscribed.unwrap_or(false) {
                        let resp = SubscriptionPayload {
                            errors: response.errors.drain(..).collect(),
                            payload: match response.data {
                                None | Some(Value::Null) if response.extensions.is_empty() => None,
                                _ => response.into(),
                            },
                        };
                        serde_json_bytes::to_value(&resp)?
                    } else {
                        serde_json_bytes::to_value(&response)?
                    };

                    let stream = Box::pin(once(ready(response)).chain(body.map(|response| {
                        if !response.errors.is_empty() {
                            Self::count_errors(&response.errors);
                        }
                        serde_json_bytes::to_value(&response)
                            .expect("response should be serializable; qed")
                    }))) as JsonStream;

                    let response = http::Response::from_parts(parts, stream);

                    Ok(JsonResponse { response, context })
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
                    Ok(JsonResponse::error_builder()
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

    fn count_errors(errors: &[graphql::Error]) {
        let mut map = HashMap::new();
        for error in errors {
            let code = error.extensions.get("code").and_then(|c| c.as_str());
            let entry = map.entry(code).or_insert(0u64);
            *entry += 1;
        }

        for (code, count) in map {
            match code {
                None => {
                    tracing::info!(monotonic_counter.apollo.router.graphql_error = count,);
                }
                Some(code) => {
                    tracing::info!(
                        monotonic_counter.apollo.router.graphql_error = count,
                        code = code
                    );
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
