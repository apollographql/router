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
            subgraph_name: self.0.subgraph_name.clone(),
            id: self.0.id.clone(),
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
        mut service: S,
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
            .with_lock(|lock| lock.contains_key::<BatchQuery>())
        {
            return service.call(request).await;
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
                                        request.subgraph_name,
                                        request.id,
                                    )
                                })
                                .map_err(|e| e.into());
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
                    let id = request.id.clone();
                    let cache_key = ((&request.subgraph_request).into(), authorization_cache_key);
                    let (res, handle) = {
                        // when _drop_signal is dropped, either by getting out of the block, returning
                        // the error from ready_oneshot or by cancellation, the drop_sentinel future will
                        // return with Err(), then we remove the entry from the wait map
                        let (_drop_signal, drop_sentinel) = oneshot::channel::<()>();
                        let handle = tokio::task::spawn(async move {
                            let _ = drop_sentinel.await;
                            let mut locked_wait_map = wait_map.lock().await;
                            locked_wait_map.remove(&cache_key);
                        });

                        (
                            service.call(request).await.map(CloneSubgraphResponse),
                            handle,
                        )
                    };

                    // Make sure that our spawned task has completed. Ignore the result to preserve
                    // existing behaviour.
                    let _ = handle.await;
                    // At this point we have removed ourselves from the wait_map, so we won't get
                    // any more receivers. If we have any receivers, let them know
                    if tx.receiver_count() > 0 {
                        // Clippy is wrong, the suggestion adds a useless clone of the error
                        #[allow(clippy::useless_asref)]
                        let broadcast_value = res
                            .as_ref()
                            .map(|response| response.clone())
                            .map_err(|e: &BoxError| e.to_string());

                        // Ignore the result of send, receivers may drop...
                        let _ = tx.send(broadcast_value);
                    }

                    return res.map(|response| {
                        SubgraphResponse::new_from_response(
                            response.0.response,
                            context,
                            response.0.subgraph_name,
                            id,
                        )
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

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let service = self.service.clone();
        let mut inner = std::mem::replace(&mut self.service, service);

        if request.operation_kind == OperationKind::Query {
            let wait_map = self.wait_map.clone();

            Box::pin(async move { Self::dedup(inner, wait_map, request).await })
        } else {
            Box::pin(async move { inner.call(request).await })
        }
    }
}

#[cfg(test)]
mod tests {

    use std::sync::Arc;
    use std::sync::atomic::AtomicU8;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use tower::Service;
    use tower::ServiceExt;

    use super::QueryDeduplicationService;
    use crate::plugin::test::MockSubgraphService;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    // Testing strategy:
    //  - We make our subgraph invocations slow (100ms) to increase our chance of a positive dedup
    //    result
    //  - We count how many times our inner service is invoked across all service invocations
    //  - We never know exactly which inner service is going to be invoked (since we are driving
    //    the service requests concurrently and in parallel), so we set times to 0..2 (== 0 or 1)
    //    for each expectation.
    //  - Every time an inner service is invoked we increment our shared counter.
    //  - If our shared counter == 1 at the end, then our test passes.
    //
    //  Note: If this test starts to fail it may be because we need to increase the sleep time for
    //  each inner service above 100ms.
    //
    #[tokio::test(flavor = "multi_thread")]
    async fn test_dedup_service() {
        let mut mock = MockSubgraphService::new();

        let inner_invocation_count = Arc::new(AtomicU8::new(0));
        let inner_invocation_count_1 = inner_invocation_count.clone();
        let inner_invocation_count_2 = inner_invocation_count.clone();
        let inner_invocation_count_3 = inner_invocation_count.clone();

        mock.expect_clone().returning(move || {
            let mut mock = MockSubgraphService::new();

            let inner_invocation_count_1 = inner_invocation_count_1.clone();
            mock.expect_clone().returning(move || {
                let mut mock = MockSubgraphService::new();
                let inner_invocation_count_1 = inner_invocation_count_1.clone();
                mock.expect_call()
                    .times(0..2)
                    .returning(move |req: SubgraphRequest| {
                        std::thread::sleep(Duration::from_millis(100));
                        inner_invocation_count_1.fetch_add(1, Ordering::Relaxed);
                        Ok(SubgraphResponse::fake_builder()
                            .context(req.context)
                            .build())
                    });
                mock
            });
            let inner_invocation_count_2 = inner_invocation_count_2.clone();
            mock.expect_call()
                .times(0..2)
                .returning(move |req: SubgraphRequest| {
                    std::thread::sleep(Duration::from_millis(100));
                    inner_invocation_count_2.fetch_add(1, Ordering::Relaxed);
                    Ok(SubgraphResponse::fake_builder()
                        .context(req.context)
                        .build())
                });
            mock
        });
        mock.expect_call()
            .times(0..2)
            .returning(move |req: SubgraphRequest| {
                std::thread::sleep(Duration::from_millis(100));
                inner_invocation_count_3.fetch_add(1, Ordering::Relaxed);
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build())
            });

        let mut svc = QueryDeduplicationService::new(mock);

        let request = SubgraphRequest::fake_builder().build();

        // Spawn our service invocations so they execute in parallel
        let fut1 = tokio::spawn(
            svc.ready()
                .await
                .expect("it is ready")
                .call(request.clone()),
        );
        let fut2 = tokio::spawn(svc.ready().await.expect("it is ready").call(request));
        let (res1, res2) = tokio::join!(fut1, fut2);

        // We don't care about our actual request/responses, we just want to make sure that
        // deduplication occurs...
        res1.expect("fut1 spawned").expect("fut1 joined");
        res2.expect("fut2 spawned").expect("fut2 joined");

        assert_eq!(1, inner_invocation_count.load(Ordering::Relaxed));
    }
}
