//! De-duplicate subgraph requests in flight. Implemented as a tower Layer.
//!
//! See [`Layer`] and [`tower::Service`] for more details.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::lock::Mutex;
use tokio::sync::broadcast::Sender;
use tokio::sync::broadcast::{self};
use tokio::sync::oneshot;
use tower::BoxError;
use tower::Layer;
use tower::ServiceExt;

use crate::batching::BatchQuery;
use crate::graphql::Request;
use crate::http_ext;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::query_planner::fetch::OperationKind;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

#[derive(Default)]
pub(crate) struct QueryDeduplicationLayer;

impl<S> Layer<S> for QueryDeduplicationLayer
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    type Service = QueryDeduplicationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        QueryDeduplicationService::new(service)
    }
}

type CacheKey = (http_ext::Request<Request>, Arc<CacheKeyMetadata>);

type WaitMap = Arc<Mutex<HashMap<CacheKey, Sender<Result<CloneSubgraphResponse, String>>>>>;

struct CloneSubgraphResponse(SubgraphResponse);

impl Clone for CloneSubgraphResponse {
    fn clone(&self) -> Self {
        Self(SubgraphResponse {
            response: http_ext::Response::from(&self.0.response).inner,
            context: self.0.context.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct QueryDeduplicationService<S: Clone> {
    service: S,
    wait_map: WaitMap,
}

impl<S> QueryDeduplicationService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    fn new(service: S) -> Self {
        QueryDeduplicationService {
            service,
            wait_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn dedup(
        service: S,
        wait_map: WaitMap,
        request: SubgraphRequest,
    ) -> Result<SubgraphResponse, BoxError> {
        // Check if the request is part of a batch. If it is, completely bypass dedup since it
        // will break any request batches which this request is part of.
        // This check is what enables Batching and Dedup to work together, so be very careful
        // before making any changes to it.
        if request
            .context
            .extensions()
            .lock()
            .contains_key::<BatchQuery>()
        {
            return service.ready_oneshot().await?.call(request).await;
        }
        loop {
            let mut locked_wait_map = wait_map.lock().await;
            let authorization_cache_key = request.authorization.clone();
            let cache_key = ((&request.subgraph_request).into(), authorization_cache_key);

            match locked_wait_map.get_mut(&cache_key) {
                Some(waiter) => {
                    // Register interest in key
                    let mut receiver = waiter.subscribe();
                    drop(locked_wait_map);

                    match receiver.recv().await {
                        Ok(value) => {
                            return value
                                .map(|response| {
                                    SubgraphResponse::new_from_response(
                                        response.0.response,
                                        request.context,
                                    )
                                })
                                .map_err(|e| e.into())
                        }
                        // there was an issue with the broadcast channel, retry fetching
                        Err(_) => continue,
                    }
                }
                None => {
                    let (tx, _rx) = broadcast::channel(1);

                    locked_wait_map.insert(cache_key, tx.clone());
                    drop(locked_wait_map);

                    let context = request.context.clone();
                    let authorization_cache_key = request.authorization.clone();
                    let cache_key = ((&request.subgraph_request).into(), authorization_cache_key);
                    let res = {
                        // when _drop_signal is dropped, either by getting out of the block, returning
                        // the error from ready_oneshot or by cancellation, the drop_sentinel future will
                        // return with Err(), then we remove the entry from the wait map
                        let (_drop_signal, drop_sentinel) = oneshot::channel::<()>();
                        tokio::task::spawn(async move {
                            let _ = drop_sentinel.await;
                            let mut locked_wait_map = wait_map.lock().await;
                            locked_wait_map.remove(&cache_key);
                        });

                        service
                            .ready_oneshot()
                            .await?
                            .call(request)
                            .await
                            .map(CloneSubgraphResponse)
                    };

                    // Let our waiters know
                    let broadcast_value = res
                        .as_ref()
                        .map(|response| response.clone())
                        .map_err(|e| e.to_string());

                    // We may get errors here, for instance if a task is cancelled,
                    // so just ignore the result of send
                    let _ = tokio::task::spawn_blocking(move || {
                        tx.send(broadcast_value)
                    }).await
                    .expect("can only fail if the task is aborted or if the internal code panics, neither is possible here; qed");

                    return res.map(|response| {
                        SubgraphResponse::new_from_response(response.0.response, context)
                    });
                }
            }
        }
    }
}

impl<S> tower::Service<SubgraphRequest> for QueryDeduplicationService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
{
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let service = self.service.clone();

        if request.operation_kind == OperationKind::Query {
            let wait_map = self.wait_map.clone();

            Box::pin(async move { Self::dedup(service, wait_map, request).await })
        } else {
            Box::pin(async move { service.oneshot(request).await })
        }
    }
}
