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

#[derive(Default)]
pub(crate) struct QueryDeduplicationLayer;

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
        QueryDeduplicationService::new(service)
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
}

impl<S> QueryDeduplicationService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
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

        let context = request.context.clone();
        let subgraph_name = request.subgraph_name.clone();
        let id = request.id.clone();
        let response = Self::join_or_start(service, &wait_map, request).await?;
        Ok(SubgraphResponse::new_from_response(
            response.0.response,
            context,
            subgraph_name,
            id,
        ))
    }

    /// Returns the fetch in flight for this request, or starts one with `service`.
    fn join_or_start(
        mut service: S,
        wait_map: &WaitMap,
        request: SubgraphRequest,
    ) -> Shared<Fetch> {
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
            return fetch;
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
        fetch
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
<<<<<<< HEAD
    use std::time::Duration;

    use futures::future::BoxFuture;
    use http::StatusCode;
    use tokio::time::Instant;
    use tower::BoxError;
    use tower::Layer;
=======
    use futures::FutureExt;
>>>>>>> origin/dev
    use tower::Service;
    use tower::ServiceExt;
    use tower_test::mock::Handle;

    use super::QueryDeduplicationService;
    use crate::plugins::traffic_shaping::timeout::SubgraphTimeoutLayer;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::subgraph;

<<<<<<< HEAD
    // Testing strategy:
    //  - Two calls with the same cache key are joined in the same task via tokio::join!.
    //    join! polls fut1 first: it locks the wait_map, inserts an entry, calls the inner
    //    service, and yields (pending on the mock response). join! then polls fut2: it finds
    //    the entry and joins the shared fetch. Both are suspended before the driver ever
    //    responds. This ordering is structural — cooperative scheduling in a single task —
    //    not a timing assumption.
    //  - The driver handles exactly one request. If dedup fails and fut2 reaches the inner
    //    service a second time, the closed handle returns an error and res2 fails.
    #[tokio::test(flavor = "multi_thread")]
=======
    #[tokio::test]
>>>>>>> origin/dev
    async fn test_dedup_service() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let mut svc = QueryDeduplicationService::new(mock);
        let request = SubgraphRequest::fake_builder().build();

        svc.ready().await.expect("it is ready");
        let mut fut1 = svc.call(request.clone());
        svc.ready().await.expect("it is ready");
        let mut fut2 = svc.call(request);

<<<<<<< HEAD
        // tokio::join! polls fut1 first. fut1 inserts a wait_map entry and yields waiting
        // for the inner service response. join! then polls fut2, which finds the entry and
        // joins the shared fetch. Both are suspended before the driver responds,
        // guaranteeing deduplication.
        let (res1, res2) = tokio::join!(fut1, fut2);
        res1.expect("fut1 joined");
        res2.expect("fut2 joined");
=======
        // Poll both callers before the mock answers: fut1 starts the fetch and fut2 subscribes
        // to its result.
        assert!(futures::poll!(&mut fut1).is_pending());
        assert!(futures::poll!(&mut fut2).is_pending());
>>>>>>> origin/dev

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
    }

    const TIMEOUT: Duration = Duration::from_millis(100);

    type Dedup = QueryDeduplicationService<subgraph::BoxCloneService>;

    /// Deduplication above a subgraph timeout, over a target the test answers by hand.
    fn dedup_above_timeout() -> (Dedup, Handle<SubgraphRequest, SubgraphResponse>) {
        let (target, handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let service =
            QueryDeduplicationService::new(SubgraphTimeoutLayer::new(TIMEOUT).layer(target));
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
