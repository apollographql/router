//! De-duplicate subgraph requests in flight. Implemented as a tower Layer.
//!
//! Identical queries in flight share one fetch. Queries that differ only in their operation name
//! count as identical, so fetch nodes that send the same query and variables to the same subgraph
//! share a fetch even though the query planner names each one differently.
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

/// The key `request` waits under for an identical fetch in flight.
///
/// The operation name is left out of the key, both the `operationName` field and the name in
/// the query text: the planner gives every fetch node its own name, so two fetch nodes that
/// send the same query with the same variables would otherwise never share a fetch. Requests
/// reach this layer as the planner wrote them, a single operation starting with
/// `query <operationName>`. A query in any other shape keeps its name in the key.
fn cache_key(request: &SubgraphRequest) -> CacheKey {
    let mut http_request: http_ext::Request<Request> = (&request.subgraph_request).into();
    let body = http_request.body_mut();
    if let (Some(name), Some(query)) = (&body.operation_name, &body.query)
        && let Some(rest) = query.strip_prefix(&format!("query {name}"))
        && !rest.starts_with(|c: char| c == '_' || c.is_ascii_alphanumeric())
    {
        body.query = Some(format!("query{rest}"));
        body.operation_name = None;
    }
    (http_request, request.authorization.clone())
}

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
            let key = cache_key(&request);

            match locked_wait_map.get_mut(&key) {
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

                    locked_wait_map.insert(key, tx.clone());
                    drop(locked_wait_map);

                    let context = request.context.clone();
                    let id = request.id.clone();
                    let key = cache_key(&request);
                    let (res, handle) = {
                        // when _drop_signal is dropped, either by getting out of the block, returning
                        // the error from ready_oneshot or by cancellation, the drop_sentinel future will
                        // return with Err(), then we remove the entry from the wait map
                        let (_drop_signal, drop_sentinel) = oneshot::channel::<()>();
                        let handle = tokio::task::spawn(async move {
                            let _ = drop_sentinel.await;
                            let mut locked_wait_map = wait_map.lock().await;
                            locked_wait_map.remove(&key);
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

<<<<<<< HEAD
    use std::sync::Arc;
    use std::sync::atomic::AtomicU8;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

=======
    use futures::FutureExt;
    use serde_json_bytes::json;
>>>>>>> 83ba67e (fix(router): ignore the operation name when deduplicating subgraph queries (#10465))
    use tower::Service;
    use tower::ServiceExt;

    use super::QueryDeduplicationService;
<<<<<<< HEAD
    use crate::plugin::test::MockSubgraphService;
=======
    use crate::batching::Batch;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt;
    use crate::query_planner::fetch::OperationKind;
>>>>>>> 83ba67e (fix(router): ignore the operation name when deduplicating subgraph queries (#10465))
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

<<<<<<< HEAD
        // Spawn our service invocations so they execute in parallel
        let fut1 = tokio::spawn(
            svc.ready()
                .await
                .expect("it is ready")
                .call(request.clone()),
        );
        let fut2 = tokio::spawn(svc.ready().await.expect("it is ready").call(request));
        let (res1, res2) = tokio::join!(fut1, fut2);
=======
    /// An entity fetch named `operation_name`, the way the query planner names each fetch node.
    fn entity_fetch(operation_name: &str, upc: &str) -> SubgraphRequest {
        let body = graphql::Request::fake_builder()
            .query(format!(
                "query {operation_name}($representations:[_Any!]!){{_entities(representations:$representations){{...on Product{{name}}}}}}"
            ))
            .operation_name(operation_name)
            .variable(
                "representations",
                json!([{ "__typename": "Product", "upc": upc }]),
            )
            .build();
        SubgraphRequest::fake_builder()
            .subgraph_request(http::Request::new(body))
            .build()
    }

    /// Sends `first` and then `second` while `first` is in flight, answers every request that
    /// reaches the subgraph, and returns how many did.
    async fn subgraph_requests_for(first: SubgraphRequest, second: SubgraphRequest) -> usize {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let mut svc = dedup_service(mock);

        svc.ready().await.expect("it is ready");
        let mut fut1 = svc.call(first);
        svc.ready().await.expect("it is ready");
        let mut fut2 = svc.call(second);
        assert!(futures::poll!(&mut fut1).is_pending());
        assert!(futures::poll!(&mut fut2).is_pending());

        let mut sent = 0;
        while let Some(Some((req, responder))) = handle.next_request().now_or_never() {
            sent += 1;
            responder.send_response(
                SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build(),
            );
        }
        fut1.await.expect("fut1 gets a response");
        fut2.await.expect("fut2 gets a response");
        sent
    }

    #[tokio::test]
    async fn queries_differing_only_in_operation_name_share_a_fetch() {
        async {
            let sent = subgraph_requests_for(
                entity_fetch("Op__products__1", "1"),
                entity_fetch("Op__products__7", "1"),
            )
            .await;
            assert_eq!(sent, 1, "the second fetch node joins the first one's fetch");

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

    #[tokio::test]
    async fn queries_with_different_variables_do_not_share_a_fetch() {
        let sent = subgraph_requests_for(
            entity_fetch("Op__products__1", "1"),
            entity_fetch("Op__products__7", "2"),
        )
        .await;
        assert_eq!(sent, 2, "each fetch node sends its own representations");
    }

    /// The cache key of a request sending `query` named `operation_name`, with fixed variables.
    fn test_cache_key(query: &str, operation_name: Option<&str>) -> super::CacheKey {
        let body = graphql::Request::fake_builder()
            .query(query)
            .and_operation_name(operation_name.map(str::to_string))
            .variable(
                "representations",
                json!([{ "__typename": "Product", "upc": "1" }]),
            )
            .build();
        super::cache_key(
            &SubgraphRequest::fake_builder()
                .subgraph_request(http::Request::new(body))
                .build(),
        )
    }

    #[test]
    fn cache_key_leaves_out_only_the_operation_name() {
        let named_a = test_cache_key(
            "query A($r:[_Any!]!){_entities(representations:$r){__typename}}",
            Some("A"),
        );
        let named_b = test_cache_key(
            "query B($r:[_Any!]!){_entities(representations:$r){__typename}}",
            Some("B"),
        );
        assert!(
            named_a == named_b,
            "queries differing only in their name share a key"
        );

        let anonymous = test_cache_key(
            "query($r:[_Any!]!){_entities(representations:$r){__typename}}",
            None,
        );
        assert!(
            anonymous == named_a,
            "an anonymous query is the same query without a name"
        );

        let other_name = test_cache_key(
            "query B($r:[_Any!]!){_entities(representations:$r){__typename}}",
            Some("A"),
        );
        assert!(
            other_name != named_a,
            "a query naming another operation keeps its name"
        );

        let other_body = test_cache_key(
            "query B($r:[_Any!]!){_entities(representations:$r){id}}",
            Some("B"),
        );
        assert!(
            other_body != named_a,
            "different query bodies don't share a key"
        );

        let longer_name = test_cache_key(
            "query Op__12($r:[_Any!]!){_entities(representations:$r){__typename}}",
            Some("Op__1"),
        );
        // What stripping `Op__1` from `Op__12` without a name boundary would leave.
        let stripped = test_cache_key(
            "query2($r:[_Any!]!){_entities(representations:$r){__typename}}",
            None,
        );
        assert!(
            longer_name != stripped,
            "the name is only stripped up to a name boundary"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mutations_bypass_deduplication() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move { respond_once(&mut handle).await });
>>>>>>> 83ba67e (fix(router): ignore the operation name when deduplicating subgraph queries (#10465))

        // We don't care about our actual request/responses, we just want to make sure that
        // deduplication occurs...
        res1.expect("fut1 spawned").expect("fut1 joined");
        res2.expect("fut2 spawned").expect("fut2 joined");

        assert_eq!(1, inner_invocation_count.load(Ordering::Relaxed));
    }
}
