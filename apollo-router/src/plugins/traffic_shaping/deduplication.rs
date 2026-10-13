//! De-duplicate subgraph requests in flight. Implemented as a tower Layer.
//!
//! Identical queries in flight share one fetch. The fetch is a [`Shared`] future that each
//! caller awaits through its own handle, so it keeps running while any caller still waits for
//! it. Cancelling the caller that started it neither cancels nor restarts it for the others.
//!
//! See [`Layer`] and [`tower::Service`] for more details.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Poll;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use futures::future::WeakShared;
use parking_lot::Mutex;
use tower::BoxError;
use tower::Layer;

use crate::batching::BatchQuery;
use crate::error::FetchError;
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
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
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
/// or is cancelled. A follower stays a follower when its leader is cancelled, because the fetch
/// it joined keeps running.
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

type Fetch = BoxFuture<'static, Result<CloneSubgraphResponse, CloneFetchError>>;

/// The fetch in flight for a cache key.
struct InFlight {
    /// Tells this fetch apart from a later one for the same key.
    id: u64,
    /// A weak handle, so the map doesn't keep the fetch alive once no caller waits for it.
    fetch: WeakShared<Fetch>,
}

type WaitMap = Arc<Mutex<HashMap<CacheKey, InFlight>>>;

static NEXT_FETCH_ID: AtomicU64 = AtomicU64::new(0);

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

/// A fetch error every caller can receive. A [`FetchError`] keeps its type, as the layers
/// above read it; any other error keeps only its message.
#[derive(Clone)]
enum CloneFetchError {
    Fetch(FetchError),
    Other(String),
}

impl From<BoxError> for CloneFetchError {
    fn from(error: BoxError) -> Self {
        match error.downcast::<FetchError>() {
            Ok(error) => Self::Fetch(*error),
            Err(error) => Self::Other(error.to_string()),
        }
    }
}

impl From<CloneFetchError> for BoxError {
    fn from(error: CloneFetchError) -> Self {
        match error {
            CloneFetchError::Fetch(error) => error.into(),
            CloneFetchError::Other(message) => message.into(),
        }
    }
}

/// Removes a fetch's wait map entry when the fetch completes or is dropped, unless a newer
/// fetch for the same key has replaced it.
struct RemoveFromWaitMap {
    wait_map: WaitMap,
    key: CacheKey,
    id: u64,
}

impl Drop for RemoveFromWaitMap {
    fn drop(&mut self) {
        let mut wait_map = self.wait_map.lock();
        if wait_map
            .get(&self.key)
            .is_some_and(|in_flight| in_flight.id == self.id)
        {
            wait_map.remove(&self.key);
        }
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
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
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

        let context = request.context.clone();
        let subgraph_name = request.subgraph_name.clone();
        let id = request.id.clone();
        let (fetch, outcome) = Self::join_or_start(service, &wait_map, request);
        recorder.outcome = Some(outcome);
        let response = fetch.await?;
        Ok(SubgraphResponse::new_from_response(
            response.0.response,
            context,
            subgraph_name,
            id,
        ))
    }

    /// Returns the fetch in flight for this request as its [`Outcome::Follower`], or starts one
    /// with `service` as its [`Outcome::Leader`].
    fn join_or_start(
        mut service: S,
        wait_map: &WaitMap,
        request: SubgraphRequest,
    ) -> (Shared<Fetch>, Outcome) {
        let key: CacheKey = (
            (&request.subgraph_request).into(),
            request.authorization.clone(),
        );
        // Don't drop a handle to a fetch while holding the lock: dropping the last handle drops
        // the fetch's `RemoveFromWaitMap`, which takes the lock.
        let mut locked_wait_map = wait_map.lock();
        if let Some(fetch) = locked_wait_map
            .get(&key)
            .and_then(|in_flight| in_flight.fetch.upgrade())
        {
            return (fetch, Outcome::Follower);
        }

        let id = NEXT_FETCH_ID.fetch_add(1, Ordering::Relaxed);
        let remove_from_wait_map = RemoveFromWaitMap {
            wait_map: wait_map.clone(),
            key: key.clone(),
            id,
        };
        let fetch = async move {
            let _remove_from_wait_map = remove_from_wait_map;
            service
                .call(request)
                .await
                .map(CloneSubgraphResponse)
                .map_err(CloneFetchError::from)
        }
        .boxed()
        .shared();
        let weak = fetch
            .downgrade()
            .expect("a fetch that was never polled has not completed");
        locked_wait_map.insert(key, InFlight { id, fetch: weak });
        (fetch, Outcome::Leader)
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
    use std::time::Duration;

    use futures::FutureExt;
    use futures::future::BoxFuture;
    use http::StatusCode;
    use tokio::time::Instant;
    use tower::BoxError;
    use tower::Layer;
    use tower::Service;
    use tower::ServiceExt;
    use tower_test::mock::Handle;

    use super::QueryDeduplicationService;
    use crate::batching::BatchQuery;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::traffic_shaping::timeout::SubgraphTimeoutLayer;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::subgraph;

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
            let batch_query = BatchQuery::for_test();
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

    // When the leader is dropped (for example because its client disconnected), the fetch it
    // started keeps running for the follower, so each is counted under its own outcome and the
    // subgraph receives one request.
    #[tokio::test(flavor = "multi_thread")]
    async fn follower_of_a_cancelled_leader_is_counted_as_follower() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

            let mut svc = dedup_service(mock);
            let request = SubgraphRequest::fake_builder().build();
            svc.ready().await.expect("it is ready");
            let mut leader = svc.call(request.clone());
            svc.ready().await.expect("it is ready");
            let mut follower = svc.call(request);

            // One poll takes the leader through the wait map and into the subgraph call, where it
            // waits for a response. One poll of the follower joins the leader's fetch.
            assert!(futures::poll!(&mut leader).is_pending());
            let (leader_request, leader_responder) =
                handle.next_request().await.expect("the leader's request");
            assert!(futures::poll!(&mut follower).is_pending());

            drop(leader);

            leader_responder.send_response(
                SubgraphResponse::fake_builder()
                    .context(leader_request.context)
                    .build(),
            );
            follower
                .await
                .expect("the follower gets the leader's response");
            assert!(
                handle.next_request().now_or_never().is_none(),
                "the follower does not send its own request"
            );

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

    const TIMEOUT: Duration = Duration::from_millis(100);

    type Dedup = QueryDeduplicationService<subgraph::BoxCloneService>;

    /// Deduplication above a subgraph timeout, over a target the test answers by hand.
    fn dedup_above_timeout() -> (Dedup, Handle<SubgraphRequest, SubgraphResponse>) {
        let (target, handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let service = QueryDeduplicationService::new(
            SubgraphTimeoutLayer::new(TIMEOUT).layer(target),
            SUBGRAPH.into(),
        );
        (service, handle)
    }

    /// Sends two identical queries, so the first starts the fetch and the second joins it.
    /// Cancels the first after `cancel_after` and returns the second.
    async fn cancel_first_of_two(
        service: &mut Dedup,
        cancel_after: Duration,
    ) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>> {
        let request = SubgraphRequest::fake_builder().build();
        service.ready().await.expect("it is ready");
        let mut first = service.call(request.clone());
        service.ready().await.expect("it is ready");
        let mut second = service.call(request);
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());

        tokio::time::advance(cancel_after).await;
        drop(first);
        second
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_continues_when_the_first_caller_is_cancelled() {
        let (mut service, mut target) = dedup_above_timeout();
        let second = cancel_first_of_two(&mut service, TIMEOUT / 2).await;

        let (request, responder) = target.next_request().await.expect("the target is called");
        responder.send_response(
            SubgraphResponse::fake_builder()
                .context(request.context)
                .build(),
        );
        let response = second.await.expect("the second caller gets a response");
        assert_eq!(response.response.status(), StatusCode::OK);

        drop(service);
        assert!(
            target.next_request().await.is_none(),
            "the target is called only once"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_keeps_its_deadline_when_the_first_caller_is_cancelled() {
        let (mut service, mut target) = dedup_above_timeout();
        let started = Instant::now();
        let second = cancel_first_of_two(&mut service, TIMEOUT / 2).await;

        let _unanswered = target.next_request().await.expect("the target is called");
        let response = second.await.expect("the timeout answers the second caller");
        assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(started.elapsed(), TIMEOUT);

        drop(service);
        assert!(
            target.next_request().await.is_none(),
            "the target is called only once"
        );
    }
}
