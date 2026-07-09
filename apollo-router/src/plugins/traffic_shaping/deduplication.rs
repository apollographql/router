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

    use std::time::Duration;

    use tower::Service;
    use tower::ServiceExt;

    use super::QueryDeduplicationService;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    // Testing strategy:
    //  - A previous version of this test relied on tokio::join! polling fut1 before fut2 and
    //    claimed that ordering was "structural" — cooperative scheduling in a single task, not
    //    a timing assumption. That claim was false: the wait_map entry inserted by fut1 is
    //    removed by a *separately spawned* cleanup task (see the `tokio::task::spawn` inside
    //    `dedup`'s `None` branch), and the mock driver here is also a separate spawned task.
    //    On the multi-thread runtime, both of those independent tasks can be scheduled on other
    //    worker threads and race ahead of the main task's join! loop. If the cleanup task wins
    //    that race and removes the wait_map entry before fut2 is ever polled, fut2 takes the
    //    `None` branch and calls the inner service a second time. Since the mock driver only
    //    answers one request before it drops its handle, that second call hits a closed
    //    channel and fails with `Closed(())`, which is exactly the intermittent CI failure this
    //    test was rewritten to fix (previously masked/"guaranteed" by an incorrect comment).
    //  - Rather than hope for a particular poll/scheduling order, we make deduplication a
    //    proven fact: we spawn both calls as independent tasks (so they really do run
    //    concurrently, as in production) and condition-wait on the actual invariant dedup
    //    depends on — that the second call has subscribed to the first call's broadcast
    //    channel — before letting the mock driver answer the single request it will ever see.
    //    Only then can there be no more races: the response cannot arrive, and the wait_map
    //    entry cannot be cleaned up, until the second call has already registered its interest.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_dedup_service() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut svc = QueryDeduplicationService::new(mock);
        let wait_map = svc.wait_map.clone();
        let request = SubgraphRequest::fake_builder().build();

        svc.ready().await.expect("it is ready");
        let fut1 = tokio::spawn(svc.call(request.clone()));

        // Condition-based wait: block until the first call has actually registered its
        // wait_map entry, rather than assuming it happened by the time we get here.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !wait_map.lock().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fut1 never registered a wait_map entry");

        svc.ready().await.expect("it is ready");
        let fut2 = tokio::spawn(svc.call(request));

        // Condition-based wait: block until the second call has subscribed to the first
        // call's broadcast channel. This is the precise invariant that makes deduplication
        // happen; proving it (instead of assuming a poll order) is what removes the race.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let subscribed = wait_map
                    .lock()
                    .await
                    .values()
                    .next()
                    .map(|tx| tx.receiver_count() >= 2)
                    .unwrap_or(false);
                if subscribed {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fut2 never subscribed to the dedup broadcast channel");

        // Only now, with deduplication provably in effect, let the driver answer the single
        // request it will ever see.
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build(),
            );
        });

        let (res1, res2) = tokio::join!(fut1, fut2);
        res1.expect("fut1 spawned").expect("fut1 joined");
        res2.expect("fut2 spawned").expect("fut2 joined");

        crate::plugin::test::await_mock_driver(driver).await;
    }
}
