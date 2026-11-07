//! De-duplicate subgraph requests in flight. Implemented as a tower Layer.
//!
//! Identical queries in flight share one fetch. The fetch is a [`Shared`] future that each
//! caller awaits through its own handle, so it keeps running while any caller still waits for
//! it. Cancelling the caller that started it neither cancels nor restarts it for the others.
//!
//! Queries that differ only in their operation name count as identical, so fetch nodes that send
//! the same query and variables to the same subgraph share a fetch even though the query planner
//! names each one differently.
//!
//! See [`Layer`] and [`tower::Service`] for more details.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::BuildHasher;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Poll;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use futures::future::WeakShared;
use http::uri::Authority;
use http::uri::PathAndQuery;
use parking_lot::Mutex;
use tower::BoxError;
use tower::Layer;

use crate::batching::BatchQuery;
use crate::error::FetchError;
use crate::graphql::Request;
use crate::http_ext;
use crate::json_ext::Object;
use crate::json_ext::Value;
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
                "subgraph.name" = self.subgraph_name.clone(),
                "apollo.router.subgraph_deduplication.outcome" = outcome
            );
        }
    }
}

/// The key `request` waits under for an identical fetch in flight: its method, HTTP version,
/// URI, headers (in order), authorization metadata and body, encoded as bytes.
///
/// Two requests share a key only if all of these are equal. Every variable-length part is
/// length-prefixed and every JSON value is tagged with its type, so no two different requests
/// encode to the same bytes. The wait map compares whole keys, so requests whose hashes collide
/// still never share a fetch.
///
/// The operation name is left out of the body, both the `operationName` field and the name in
/// the query text: the planner gives every fetch node its own name, so two fetch nodes that
/// send the same query with the same variables would otherwise never share a fetch. Requests
/// reach this layer as the planner wrote them, a single operation starting with
/// `query <operationName>`. A query in any other shape keeps its name in the key.
fn cache_key(request: &SubgraphRequest) -> Vec<u8> {
    let http_request = &request.subgraph_request;
    // Large enough for most subgraph requests, so the key is built without growing.
    let mut key = KeyWriter(Vec::with_capacity(1024));
    key.bytes(http_request.method().as_str().as_bytes());
    key.0.push(version_tag(http_request.version()));
    let uri = http_request.uri();
    key.optional(uri.scheme_str());
    key.optional(uri.authority().map(Authority::as_str));
    key.optional(uri.path_and_query().map(PathAndQuery::as_str));
    key.len(http_request.headers().len());
    for (name, value) in http_request.headers() {
        key.bytes(name.as_str().as_bytes());
        key.bytes(value.as_bytes());
    }
    // Exhaustive, so a field added to the metadata can't be left out of the key unnoticed.
    let CacheKeyMetadata {
        is_authenticated,
        scopes,
        policies,
    } = &*request.authorization;
    key.0.push((*is_authenticated).into());
    key.strings(scopes);
    key.strings(policies);
    key.body(http_request.body());
    key.0
}

/// Appends length-prefixed parts of a cache key.
struct KeyWriter(Vec<u8>);

impl KeyWriter {
    fn len(&mut self, len: usize) {
        self.0.extend_from_slice(&len.to_le_bytes());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.len(bytes.len());
        self.0.extend_from_slice(bytes);
    }

    fn optional(&mut self, part: Option<&str>) {
        match part {
            Some(part) => {
                self.0.push(1);
                self.bytes(part.as_bytes());
            }
            None => self.0.push(0),
        }
    }

    fn strings(&mut self, strings: &[String]) {
        self.len(strings.len());
        for string in strings {
            self.bytes(string.as_bytes());
        }
    }

    /// Appends a request body. Two bodies get the same bytes exactly when they serialize to the
    /// same JSON, except that the operation name is left out as [`cache_key`] describes.
    fn body(&mut self, body: &Request) {
        // Exhaustive, so a field added to the request can't be left out of the key unnoticed.
        let Request {
            query,
            operation_name,
            variables,
            extensions,
        } = body;
        let unnamed = match (query, operation_name) {
            (Some(query), Some(name)) => query_after_name(query, name),
            _ => None,
        };
        match unnamed {
            // The same bytes as this query sent without a name: `query` followed by the rest.
            Some(rest) => {
                self.0.push(1);
                self.len("query".len() + rest.len());
                self.0.extend_from_slice(b"query");
                self.0.extend_from_slice(rest.as_bytes());
                self.optional(None);
            }
            None => {
                self.optional(query.as_deref());
                self.optional(operation_name.as_deref());
            }
        }
        self.object(variables);
        self.object(extensions);
    }

    fn object(&mut self, object: &Object) {
        self.len(object.len());
        for (name, value) in object {
            self.bytes(name.as_str().as_bytes());
            self.value(value);
        }
    }

    fn value(&mut self, value: &Value) {
        match value {
            Value::Null => self.0.push(0),
            Value::Bool(false) => self.0.push(1),
            Value::Bool(true) => self.0.push(2),
            Value::Number(number) => {
                if let Some(number) = number.as_u64() {
                    self.0.push(3);
                    self.0.extend_from_slice(&number.to_le_bytes());
                } else if let Some(number) = number.as_i64() {
                    self.0.push(4);
                    self.0.extend_from_slice(&number.to_le_bytes());
                } else {
                    // A float, as the JSON text it serializes to.
                    self.0.push(5);
                    self.bytes(number.to_string().as_bytes());
                }
            }
            Value::String(string) => {
                self.0.push(6);
                self.bytes(string.as_str().as_bytes());
            }
            Value::Array(items) => {
                self.0.push(7);
                self.len(items.len());
                for item in items {
                    self.value(item);
                }
            }
            Value::Object(object) => {
                self.0.push(8);
                self.object(object);
            }
        }
    }
}

fn version_tag(version: http::Version) -> u8 {
    match version {
        http::Version::HTTP_09 => 0,
        http::Version::HTTP_10 => 1,
        http::Version::HTTP_11 => 2,
        http::Version::HTTP_2 => 3,
        http::Version::HTTP_3 => 4,
        _ => u8::MAX,
    }
}

/// The rest of `query` after `query <name>`, if it starts that way.
fn query_after_name<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .strip_prefix("query ")?
        .strip_prefix(name)
        .filter(|rest| !rest.starts_with(|c: char| c == '_' || c.is_ascii_alphanumeric()))
}

type Fetch = BoxFuture<'static, Result<CloneSubgraphResponse, CloneFetchError>>;

/// The fetch in flight for a cache key.
struct InFlight {
    /// Tells this fetch apart from a later one for the same key.
    id: u64,
    /// A weak handle, so the map doesn't keep the fetch alive once no caller waits for it.
    fetch: WeakShared<Fetch>,
}

/// The fetches in flight, by [`cache_key`]. `H` hashes keys; tests replace it with a hasher
/// that makes every key collide.
type WaitMap<H> = Arc<Mutex<HashMap<Arc<[u8]>, InFlight, H>>>;

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
struct RemoveFromWaitMap<H: BuildHasher> {
    wait_map: WaitMap<H>,
    key: Arc<[u8]>,
    id: u64,
}

impl<H: BuildHasher> Drop for RemoveFromWaitMap<H> {
    fn drop(&mut self) {
        let mut wait_map = self.wait_map.lock();
        if let Entry::Occupied(entry) = wait_map.entry(self.key.clone())
            && entry.get().id == self.id
        {
            entry.remove();
        }
    }
}

#[derive(Clone)]
pub(crate) struct QueryDeduplicationService<S: Clone, H = ahash::RandomState> {
    service: S,
    wait_map: WaitMap<H>,
    subgraph_name: Arc<str>,
}

impl<S, H> QueryDeduplicationService<S, H>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
    H: BuildHasher + Default + Send + Sync + 'static,
{
    fn new(service: S, subgraph_name: Arc<str>) -> Self {
        QueryDeduplicationService {
            service,
            wait_map: Arc::new(Mutex::new(HashMap::default())),
            subgraph_name,
        }
    }

    async fn dedup(
        mut service: S,
        wait_map: WaitMap<H>,
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
        wait_map: &WaitMap<H>,
        request: SubgraphRequest,
    ) -> (Shared<Fetch>, Outcome) {
        let key = cache_key(&request);
        // Don't drop a handle to a fetch while holding the lock: dropping the last handle drops
        // the fetch's `RemoveFromWaitMap`, which takes the lock.
        let mut locked_wait_map = wait_map.lock();
        if let Some(fetch) = locked_wait_map
            .get(key.as_slice())
            .and_then(|in_flight| in_flight.fetch.upgrade())
        {
            return (fetch, Outcome::Follower);
        }

        let key: Arc<[u8]> = key.into();
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

impl<S, H> tower::Service<SubgraphRequest> for QueryDeduplicationService<S, H>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
    H: BuildHasher + Default + Send + Sync + 'static,
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
    use std::hash::BuildHasher;
    use std::hash::BuildHasherDefault;
    use std::hash::Hasher;
    use std::sync::Arc;
    use std::time::Duration;

    use futures::FutureExt;
    use futures::future::BoxFuture;
    use http::StatusCode;
    use serde_json_bytes::json;
    use tokio::time::Instant;
    use tower::BoxError;
    use tower::Layer;
    use tower::Service;
    use tower::ServiceExt;
    use tower_test::mock::Handle;

    use super::QueryDeduplicationService;
    use crate::batching::BatchQuery;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::authorization::CacheKeyMetadata;
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
        subgraph_requests_hashed_by::<ahash::RandomState>(first, second).await
    }

    /// [`subgraph_requests_for`] with a wait map that hashes keys with `H`.
    async fn subgraph_requests_hashed_by<H>(
        first: SubgraphRequest,
        second: SubgraphRequest,
    ) -> usize
    where
        H: BuildHasher + Default + Send + Sync + 'static,
    {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let mut svc = QueryDeduplicationService::<_, H>::new(mock, SUBGRAPH.into());

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

    /// Hashes every key to the same value, so every wait map lookup collides.
    #[derive(Default)]
    struct SameHash;

    impl Hasher for SameHash {
        fn finish(&self) -> u64 {
            0
        }

        fn write(&mut self, _bytes: &[u8]) {}
    }

    type CollidingHashes = BuildHasherDefault<SameHash>;

    /// An entity fetch whose HTTP request is built by `http_request`.
    fn entity_fetch_with(http_request: http::request::Builder) -> SubgraphRequest {
        let body = entity_fetch("Op__products__1", "1")
            .subgraph_request
            .into_body();
        SubgraphRequest::fake_builder()
            .subgraph_request(http_request.body(body).expect("a valid request"))
            .build()
    }

    #[tokio::test]
    async fn identical_queries_share_a_fetch_when_hashes_collide() {
        let sent = subgraph_requests_hashed_by::<CollidingHashes>(
            entity_fetch("Op__products__1", "1"),
            entity_fetch("Op__products__1", "1"),
        )
        .await;
        assert_eq!(sent, 1);
    }

    #[tokio::test]
    async fn queries_that_differ_never_share_a_fetch_when_hashes_collide() {
        let mut authenticated = entity_fetch("Op__products__1", "1");
        authenticated.authorization = Arc::new(CacheKeyMetadata {
            is_authenticated: true,
            ..Default::default()
        });
        let cases = [
            (
                "a header",
                entity_fetch_with(http::Request::builder().header("authorization", "Bearer a")),
                entity_fetch_with(http::Request::builder().header("authorization", "Bearer b")),
            ),
            (
                "a variable",
                entity_fetch("Op__products__1", "1"),
                entity_fetch("Op__products__1", "2"),
            ),
            (
                "the URI",
                entity_fetch_with(http::Request::builder().uri("http://products-a/graphql")),
                entity_fetch_with(http::Request::builder().uri("http://products-b/graphql")),
            ),
            (
                "the authorization metadata",
                entity_fetch("Op__products__1", "1"),
                authenticated,
            ),
        ];
        for (difference, first, second) in cases {
            let sent = subgraph_requests_hashed_by::<CollidingHashes>(first, second).await;
            assert_eq!(
                sent, 2,
                "queries differing in {difference} each reach the subgraph"
            );
        }
    }

    /// The cache key of a request sending `body`.
    fn key_of(body: graphql::Request) -> Vec<u8> {
        super::cache_key(
            &SubgraphRequest::fake_builder()
                .subgraph_request(http::Request::new(body))
                .build(),
        )
    }

    /// The cache key of a request sending `query` named `operation_name`, with fixed variables.
    fn test_cache_key(query: &str, operation_name: Option<&str>) -> Vec<u8> {
        key_of(
            graphql::Request::fake_builder()
                .query(query)
                .and_operation_name(operation_name.map(str::to_string))
                .variable(
                    "representations",
                    json!([{ "__typename": "Product", "upc": "1" }]),
                )
                .build(),
        )
    }

    #[test]
    fn cache_key_tells_apart_variables_that_serialize_differently() {
        let key_with = |value: serde_json_bytes::Value| {
            key_of(
                graphql::Request::fake_builder()
                    .query("query($v:JSON){f(v:$v)}")
                    .variable("v", value)
                    .build(),
            )
        };
        let pairs = [
            (json!(1), json!("1")),
            (json!(1), json!(1.0)),
            (json!(-1), json!(u64::MAX)),
            (json!(0.0), json!(-0.0)),
            (json!(null), json!(false)),
            (json!(["a", "b"]), json!(["ab"])),
            (json!({ "a": "bc" }), json!({ "ab": "c" })),
            (json!([[]]), json!([{}])),
        ];
        for (first, second) in pairs {
            assert!(
                key_with(first.clone()) != key_with(second.clone()),
                "{first} and {second} get different keys"
            );
        }
        assert!(
            key_with(json!({ "n": 1.5 })) == key_with(json!({ "n": 1.5 })),
            "equal variables share a key"
        );
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
