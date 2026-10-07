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

pub(crate) struct QueryDeduplicationLayer {
    subgraph_name: Arc<str>,
}

impl QueryDeduplicationLayer {
    pub(crate) fn new(subgraph_name: &str) -> Self {
        Self {
            subgraph_name: subgraph_name.into(),
        }
    }
}

impl<S> Layer<S> for QueryDeduplicationLayer
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    type Service = QueryDeduplicationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        QueryDeduplicationService::new(service, self.subgraph_name.clone())
    }
}

/// How the deduplication layer handled a subgraph request.
#[derive(Copy, Clone, Debug, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum Outcome {
    /// The request was forwarded to the rest of the subgraph pipeline; identical requests
    /// arriving while it is in flight share its response.
    Leader,
    /// The request reused the response of an identical in-flight request instead of being sent.
    Follower,
    /// The request is a query that is part of a batch, so it was never considered for
    /// deduplication.
    BypassedBatch,
    /// The request is a mutation or subscription, so it was never considered for deduplication.
    /// The operation kind is checked first, so this applies whether or not it is batched.
    BypassedOperation,
}

impl_otel_value_from_static_str!(Outcome);

/// Counts a request on `apollo.router.subgraph_deduplication.requests` once, when it finishes
/// or is cancelled, using the last outcome it was given. A follower that retries after its
/// leader went away is therefore counted under the role it ends with, not twice.
struct OutcomeRecorder {
    subgraph_name: Arc<str>,
    outcome: Option<Outcome>,
}

impl OutcomeRecorder {
    fn new(subgraph_name: Arc<str>) -> Self {
        Self {
            subgraph_name,
            outcome: None,
        }
    }
}

impl Drop for OutcomeRecorder {
    fn drop(&mut self) {
        // A request cancelled before it was classified (still waiting for the wait map lock)
        // is not counted.
        if let Some(outcome) = self.outcome {
            u64_counter_with_unit!(
                "apollo.router.subgraph_deduplication.requests",
                "Subgraph requests that reached query deduplication, by outcome",
                "{request}",
                1,
                "subgraph.name" = self.subgraph_name.to_string(),
                "apollo.router.subgraph_deduplication.outcome" = outcome
            );
        }
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
    subgraph_name: Arc<str>,
}

impl<S> QueryDeduplicationService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    fn new(service: S, subgraph_name: Arc<str>) -> Self {
        QueryDeduplicationService {
            service,
            wait_map: Arc::new(Mutex::new(HashMap::new())),
            subgraph_name,
        }
    }

    async fn dedup(
        mut service: S,
        wait_map: WaitMap,
        request: SubgraphRequest,
        recorder: &mut OutcomeRecorder,
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
            recorder.outcome = Some(Outcome::BypassedBatch);
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
                    recorder.outcome = Some(Outcome::Follower);

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
                    recorder.outcome = Some(Outcome::Leader);

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
        let wait_map = self.wait_map.clone();
        let subgraph_name = self.subgraph_name.clone();

        Box::pin(async move {
            // Created inside the future so that a future which is never polled records nothing.
            let mut recorder = OutcomeRecorder::new(subgraph_name);
            if request.operation_kind == OperationKind::Query {
                Self::dedup(inner, wait_map, request, &mut recorder).await
            } else {
                recorder.outcome = Some(Outcome::BypassedOperation);
                inner.call(request).await
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::FutureExt;
    use tower::Service;
    use tower::ServiceExt;

    use super::QueryDeduplicationService;
    use crate::batching::Batch;
    use crate::metrics::FutureMetricsExt;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    const SUBGRAPH: &str = "products";

    fn dedup_service(
        mock: tower_test::mock::Mock<SubgraphRequest, SubgraphResponse>,
    ) -> QueryDeduplicationService<tower_test::mock::Mock<SubgraphRequest, SubgraphResponse>> {
        QueryDeduplicationService::new(mock, SUBGRAPH.into())
    }

    /// Answers the next request that reaches the mock subgraph.
    async fn respond_once(
        handle: &mut tower_test::mock::Handle<SubgraphRequest, SubgraphResponse>,
    ) {
        let (req, responder) = handle.next_request().await.expect("a subgraph request");
        responder.send_response(
            SubgraphResponse::fake_builder()
                .context(req.context)
                .build(),
        );
    }

    #[tokio::test]
    async fn test_dedup_service() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let mut svc = dedup_service(mock);
            let request = SubgraphRequest::fake_builder().build();

            svc.ready().await.expect("it is ready");
            let mut fut1 = svc.call(request.clone());
            svc.ready().await.expect("it is ready");
            let mut fut2 = svc.call(request);

            // Poll both callers before the mock answers: fut1 starts the fetch and fut2
            // subscribes to its result.
            assert!(futures::poll!(&mut fut1).is_pending());
            assert!(futures::poll!(&mut fut2).is_pending());

            let (req, responder) = handle.next_request().await.expect("the mock is called");
            assert!(
                handle.next_request().now_or_never().is_none(),
                "the second caller joins the first fetch instead of calling the mock"
            );
            responder.send_response(
                SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build(),
            );

            fut1.await.expect("fut1 gets the response");
            fut2.await.expect("fut2 gets the response");

            assert_counter!(
                "apollo.router.subgraph_deduplication.requests",
                1,
                "subgraph.name" = SUBGRAPH,
                "apollo.router.subgraph_deduplication.outcome" = "leader"
            );
            assert_counter!(
                "apollo.router.subgraph_deduplication.requests",
                1,
                "subgraph.name" = SUBGRAPH,
                "apollo.router.subgraph_deduplication.outcome" = "follower"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mutations_bypass_deduplication() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move { respond_once(&mut handle).await });

            let mut svc = dedup_service(mock);
            let request = SubgraphRequest::fake_builder()
                .operation_kind(OperationKind::Mutation)
                .build();
            svc.ready().await.expect("it is ready");
            svc.call(request).await.expect("the mutation is sent");

            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "apollo.router.subgraph_deduplication.requests",
                1,
                "subgraph.name" = SUBGRAPH,
                "apollo.router.subgraph_deduplication.outcome" = "bypassed_operation"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batched_queries_bypass_deduplication() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move { respond_once(&mut handle).await });

            let mut svc = dedup_service(mock);
            let request = SubgraphRequest::fake_builder().build();
            let batch = Arc::new(Batch::spawn_handler(1));
            let batch_query = Batch::query_for_index(batch, 0).expect("a valid batch index");
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(batch_query));
            svc.ready().await.expect("it is ready");
            svc.call(request).await.expect("the query is sent");

            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "apollo.router.subgraph_deduplication.requests",
                1,
                "subgraph.name" = SUBGRAPH,
                "apollo.router.subgraph_deduplication.outcome" = "bypassed_batch"
            );
        }
        .with_metrics()
        .await;
    }

    // When the leader is dropped (for example by the timeout layer above), the waiting follower's
    // broadcast closes and it retries. With no leader left it becomes the leader itself. Both the
    // cancelled leader and the retried request are counted as leaders, and nothing is counted as a
    // follower.
    #[tokio::test(flavor = "multi_thread")]
    async fn follower_of_a_cancelled_leader_is_counted_as_leader() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

            let mut svc = dedup_service(mock);
            let request = SubgraphRequest::fake_builder().build();
            svc.ready().await.expect("it is ready");
            let mut leader = svc.call(request.clone());
            svc.ready().await.expect("it is ready");
            let mut follower = svc.call(request);

            // One poll takes the leader through the wait map and into the subgraph call, where it
            // waits for a response. One poll of the follower finds the leader's entry and
            // subscribes to its broadcast.
            assert!(futures::poll!(&mut leader).is_pending());
            let (_leader_request, _leader_responder) =
                handle.next_request().await.expect("the leader's request");
            assert!(futures::poll!(&mut follower).is_pending());

            drop(leader);

            // The follower retries as the new leader and sends its own request.
            let (response, ()) = tokio::join!(follower, respond_once(&mut handle));
            response.expect("the retried request is answered");

            assert_counter!(
                "apollo.router.subgraph_deduplication.requests",
                2,
                "subgraph.name" = SUBGRAPH,
                "apollo.router.subgraph_deduplication.outcome" = "leader"
            );
            assert_counter_not_exists!(
                "apollo.router.subgraph_deduplication.requests",
                u64,
                "subgraph.name" = SUBGRAPH,
                "apollo.router.subgraph_deduplication.outcome" = "follower"
            );
        }
        .with_metrics()
        .await;
    }
}
