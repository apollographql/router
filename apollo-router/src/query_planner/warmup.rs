use std::sync::Arc;

use futures::future::BoxFuture;
use rand::seq::SliceRandom as _;
use tower::ServiceExt as _;

use crate::Context;
use crate::compute_job::ComputeJobType;
use crate::compute_job::MaybeBackPressureError;
use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
use crate::error::CacheResolverError;
use crate::plugins::authorization;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::telemetry::utils::Timer;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::QueryPlanningOutcome;
use crate::services::CachingRequest;
use crate::services::PlanOptions;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::query_parsing;

pub(crate) type BoxCloneService =
    tower::util::BoxCloneService<WarmupRequest, (), CacheResolverError>;

/// Where an operation being warmed up came from. Used to attribute warm-up metrics.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum WarmUpSource {
    /// An operation from the persisted-query manifest.
    PersistedQuery,
    /// A "hot" operation carried over from the previous in-memory cache.
    Cache,
}

/// The phase of warm-up at which a temporary/backpressure error occurred. Used to attribute the
/// warm-up backpressure metric.
#[derive(Copy, Clone, Debug, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum WarmUpPhase {
    Parse,
    Plan,
}

impl_otel_value_from_static_str!(WarmUpSource, WarmUpPhase);

/// Record the terminal outcome of warming up a single operation, attributed by source.
fn record_warmup_outcome(source: WarmUpSource, outcome: QueryPlanningOutcome) {
    u64_counter_with_unit!(
        "apollo.router.query_planning.warmup.operations",
        "Number of operations processed during query planner warm-up, by outcome and source",
        "{operation}",
        1,
        outcome = outcome,
        source = source
    );
}

/// Record that warm-up skipped an operation after hitting a temporary/backpressure error,
/// attributed by source and the phase (parse vs. plan) at which it occurred. Router 3.0 doesn't
/// retry warm-up under backpressure, so this is a terminal, not a transient, event.
fn record_warmup_backpressure(source: WarmUpSource, phase: WarmUpPhase) {
    u64_counter_with_unit!(
        "apollo.router.query_planning.warmup.backpressure",
        "Number of operations query planner warm-up skipped after a temporary/backpressure error",
        "{event}",
        1,
        source = source,
        phase = phase
    );
}

/// Record how many operations warm-up intends to plan for a given source. Emitted even when
/// `expected` is zero so the series always exists and coverage can be computed per source.
fn record_warmup_expected(expected: usize, source: WarmUpSource) {
    u64_counter_with_unit!(
        "apollo.router.query_planning.warmup.operations.expected",
        "Number of operations the query planner warm-up intended to plan",
        "{operation}",
        expected as u64,
        source = source
    );
}

#[derive(Debug)]
pub(crate) struct WarmupRequest {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,

    // XXX(@goto-bus-stop): tbh, I'm not sure that we should be warming queries that have
    // custom authorization metadata, how likely is it that they're gonna be reused?
    /// Authorization metadata, only use for re-warming queries from in-memory cache.
    pub(crate) metadata: Option<authorization::CacheKeyMetadata>,
    /// Query plan options, only use for re-warming queries from in-memory cache.
    pub(crate) plan_options: Option<PlanOptions>,
    /// Where this operation came from. Used to attribute warm-up metrics.
    pub(crate) source: WarmUpSource,
}

/// Query parsing specifically for cache warmup.
///
/// XXX(@goto-bus-stop): Ideally we would do this with the ParseQueryLayer that
/// we also use for parsing inside the router service, but it requires a
/// different request/response type.
#[derive(Clone)]
pub(crate) struct WarmupParseQueryLayer {
    query_parsing_service: query_parsing::BoxCloneService,
}

impl WarmupParseQueryLayer {
    pub(crate) fn new(query_parsing_service: query_parsing::BoxCloneService) -> Self {
        Self {
            query_parsing_service,
        }
    }
}

impl<S> tower::Layer<S> for WarmupParseQueryLayer {
    type Service = WarmupParseQueryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        WarmupParseQueryService {
            query_parsing_service: self.query_parsing_service.clone(),
            inner,
        }
    }
}

#[derive(Clone)]
pub(crate) struct WarmupParseQueryService<S> {
    query_parsing_service: query_parsing::BoxCloneService,
    inner: S,
}

impl<S> tower::Service<WarmupRequest> for WarmupParseQueryService<S>
where
    S: tower::Service<CachingRequest, Error = CacheResolverError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = CacheResolverError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::ready!(self.query_parsing_service.poll_ready(cx)).map_err(|err| match err {
            MaybeBackPressureError::PermanentError(err) => {
                CacheResolverError::RetrievalError(Arc::new(err.into()))
            }
            MaybeBackPressureError::TemporaryError(err) => CacheResolverError::Backpressure(err),
        })?;
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: WarmupRequest) -> Self::Future {
        let query_parsing_service = self.query_parsing_service.clone();
        let mut query_parsing_service =
            std::mem::replace(&mut self.query_parsing_service, query_parsing_service);
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let source = req.source;

        Box::pin(async move {
            let result = query_parsing_service
                .call(query_parsing::Request::new_warmup(
                    req.query.clone(),
                    req.operation_name.clone(),
                ))
                .await;

            let document = match result {
                Ok(document) => document,
                Err(MaybeBackPressureError::PermanentError(err)) => {
                    return Err(CacheResolverError::RetrievalError(Arc::new(err.into())));
                }
                Err(MaybeBackPressureError::TemporaryError(err)) => {
                    // if the compute pool has no room, we skip the
                    // operation instead of queueing more work onto an already-overloaded pool.
                    record_warmup_backpressure(source, WarmUpPhase::Parse);
                    return Err(CacheResolverError::Backpressure(err));
                }
            };

            let context = Context::new();
            if let Some(plan_options) = req.plan_options {
                context
                    .insert(LABELS_TO_OVERRIDE_KEY, plan_options.override_conditions)
                    .expect("JSON array will round-trip fine");
            }
            context.extensions().with_lock(|lock| {
                lock.insert(document);
                if let Some(metadata) = req.metadata {
                    lock.insert(metadata);
                }
                lock.insert(ComputeJobType::QueryPlanningWarmup);
            });

            let result = inner
                .call(CachingRequest {
                    query: req.query,
                    operation_name: req.operation_name,
                    context: context.clone(),
                })
                .await;

            match &result {
                Ok(_) => {
                    record_warmup_outcome(source, QueryPlanningOutcome::Success);
                }
                Err(CacheResolverError::RetrievalError(e)) => {
                    record_warmup_outcome(source, QueryPlanningOutcome::from(e.as_ref()));
                }
                Err(CacheResolverError::Backpressure(_)) => {
                    record_warmup_backpressure(source, WarmUpPhase::Plan);
                }
            }

            result
        })
    }
}

/// Determine the queries to warm up based on:
/// - The previous in-memory cache and a maximum number of queries to use from this cache
/// - Known persisted queries
pub(crate) async fn queries_to_warm_up(
    previous_cache: Option<InMemoryQueryPlanCache>,
    max_cached_queries: Option<usize>,
    persisted_queries_operations: Option<Vec<String>>,
    experimental_pql_prewarm: &PersistedQueriesPrewarmQueryPlanCache,
) -> Vec<WarmupRequest> {
    let mut cache_keys = match previous_cache {
        Some(ref previous_cache) => {
            let cache = previous_cache.lock().await;
            let max_cached_queries = max_cached_queries.unwrap_or(cache.len() / 3);

            cache
                .iter()
                .map(|(key, _)| WarmupRequest {
                    query: key.query.clone(),
                    operation_name: key.operation.clone(),
                    metadata: Some(key.metadata.clone()),
                    plan_options: Some(key.plan_options.clone()),
                    source: WarmUpSource::Cache,
                })
                .take(max_cached_queries)
                .collect::<Vec<_>>()
        }
        None => Vec::new(),
    };
    cache_keys.shuffle(&mut rand::rng());

    let should_warm_with_pqs = (experimental_pql_prewarm.on_startup && previous_cache.is_none())
        || (experimental_pql_prewarm.on_reload && previous_cache.is_some());

    let mut persisted_query_keys = match persisted_queries_operations {
        Some(queries) if should_warm_with_pqs => queries
            .into_iter()
            .map(|query| WarmupRequest {
                query,
                operation_name: None,
                metadata: None,
                plan_options: None,
                source: WarmUpSource::PersistedQuery,
            })
            .collect(),
        _ => Vec::new(),
    };
    persisted_query_keys.shuffle(&mut rand::rng());

    // Record how many operations warm-up intends to plan, per source, so coverage
    // (planned / expected) can be computed from the warm-up outcome metric. Emitted even
    // when zero: "nothing expected from this source" is itself a meaningful signal.
    record_warmup_expected(cache_keys.len(), WarmUpSource::Cache);
    record_warmup_expected(persisted_query_keys.len(), WarmUpSource::PersistedQuery);

    // persisted queries are added first because they should get a lower priority in the LRU cache,
    // since a lot of them may be there to support old clients
    let mut all_keys = persisted_query_keys;
    all_keys.extend(cache_keys);

    all_keys
}

/// Warm up the cache inside the given service by pushing requests through.
pub(crate) async fn warm_up(
    mut service: impl tower::Service<WarmupRequest, Response = (), Error = CacheResolverError>,
    requests: Vec<WarmupRequest>,
) {
    let _timer = Timer::new(|duration| {
        f64_histogram_with_unit!(
            "apollo.router.query_planning.warmup.duration",
            "Time spent warming up the query planner queries in seconds",
            "s",
            duration.as_secs_f64()
        );
    });

    let mut count = 0;

    for request in requests {
        let Ok(service) = service.ready().await else {
            // The service is no longer usable for some reason, we just bail out
            break;
        };
        match service.call(request).await {
            Ok(()) => {
                count += 1;
            }
            Err(CacheResolverError::RetrievalError(_)) => {
                count += 1;
            }
            Err(_) => {
                // We just ignore temporary errors. The query will be planned when it is actually
                // used by a client.
            }
        }
    }

    tracing::debug!("warmed up the query planner cache with {count} queries planned");
}

/// Warms up the query plan cache behind `warmup_query_planner_service`. Collects the
/// operations to plan with [`queries_to_warm_up`], then attempts to plan each of them
/// with [`warm_up`].
pub(crate) async fn warm_up_query_planner(
    warmup_query_planner_service: BoxCloneService,
    persisted_queries: &PersistedQueryExpander,
    previous_cache: Option<InMemoryQueryPlanCache>,
    max_cached_queries: Option<usize>,
    experimental_pql_prewarm: &PersistedQueriesPrewarmQueryPlanCache,
) {
    let requests = queries_to_warm_up(
        previous_cache,
        max_cached_queries,
        persisted_queries.all_operations(),
        experimental_pql_prewarm,
    )
    .await;

    if !requests.is_empty() {
        tracing::info!(
            "warming up the query plan cache with {} queries, this might take a while",
            requests.len(),
        );
    }

    warm_up(warmup_query_planner_service, requests).await;
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use crate::Configuration;
    use crate::cache::storage::CacheStorage;
    use crate::compute_job::MaybeBackPressureError;
    use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
    use crate::error::CacheResolverError;
    use crate::error::QueryPlannerError;
    use crate::metrics::FutureMetricsExt as _;
    use crate::query_planner::CachingQueryKey;
    use crate::query_planner::ConfigModeHash;
    use crate::query_planner::InMemoryQueryPlanCache;
    use crate::query_planner::QueryPlan;
    use crate::query_planner::warmup::WarmUpSource;
    use crate::query_planner::warmup::WarmupParseQueryLayer;
    use crate::query_planner::warmup::WarmupRequest;
    use crate::services::CachingRequest;
    use crate::services::QueryPlannerContent;
    use crate::services::query_parsing;
    use crate::services::query_parsing::ParsedDocument;
    use crate::spec::Schema;
    use crate::spec::SchemaHash;

    fn downcast_parsing_mock_err(err: tower::BoxError) -> query_parsing::ServiceError {
        *err.downcast()
            .expect("mock only produces query parsing errors")
    }

    fn downcast_planning_mock_err(err: tower::BoxError) -> CacheResolverError {
        *err.downcast()
            .expect("mock only produces CacheResolverErrors")
    }

    /// Returns an in-memory cache with the given queries inside.
    async fn prepopulated_cache<Q: AsRef<str>>(
        queries: impl Iterator<Item = Q>,
    ) -> InMemoryQueryPlanCache {
        let cache =
            CacheStorage::new_in_memory(NonZeroUsize::new(100).unwrap(), "test").in_memory_cache();

        // Moot for these tests
        let schema_hash = SchemaHash::new("");

        fn empty_query_plan() -> Result<QueryPlannerContent, Arc<QueryPlannerError>> {
            Ok(Arc::new(QueryPlan::fake_new(None, None)))
        }

        {
            let mut guard = cache.lock().await;
            for query in queries {
                // XXX(@goto-bus-stop): preferable if we didn't have to construct this manually,
                // instead used the CachingQueryPlanner public API. But then we need a bunch more
                // plumbing :/
                guard.get_or_insert(
                    CachingQueryKey {
                        query: query.as_ref().to_string(),
                        operation: None,
                        hash: Arc::new(schema_hash.operation_hash(query.as_ref(), None)),
                        schema_id: schema_hash.clone(),
                        metadata: Default::default(),
                        plan_options: Default::default(),
                        config_mode_hash: ConfigModeHash::from_configuration(&Default::default())
                            .into(),
                    },
                    empty_query_plan,
                );
            }
        }

        cache
    }

    #[tokio::test]
    async fn warm_up_pqs_on_startup() {
        async {
            let mut requests = super::queries_to_warm_up(
                None,
                None,
                Some(vec![
                    "{ me { username } }".to_string(),
                    "{ topProducts { upc } }".to_string(),
                ]),
                &PersistedQueriesPrewarmQueryPlanCache {
                    on_startup: true,
                    on_reload: false,
                },
            )
            .await;

            requests.sort_by(|a, b| a.query.cmp(&b.query));

            assert_eq!(requests[0].query, "{ me { username } }");
            assert_eq!(requests[0].metadata, None);
            assert_eq!(requests[0].plan_options, None);
            assert_eq!(requests[1].query, "{ topProducts { upc } }");
            assert_eq!(requests[1].metadata, None);
            assert_eq!(requests[1].plan_options, None);

            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                0,
                "source" = "cache"
            );
            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                2,
                "source" = "persisted_query"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warm_up_pqs_on_reload() {
        async {
            let pqs = vec![
                "{ me { username } }".to_string(),
                "{ topProducts { upc } }".to_string(),
            ];

            let requests = super::queries_to_warm_up(
                None,
                None,
                Some(pqs.clone()),
                &PersistedQueriesPrewarmQueryPlanCache {
                    on_startup: false,
                    on_reload: true,
                },
            )
            .await;
            assert!(requests.is_empty());

            let requests = super::queries_to_warm_up(
                Some(
                    CacheStorage::new_in_memory(NonZeroUsize::new(1).unwrap(), "test")
                        .in_memory_cache(),
                ),
                None,
                Some(pqs.clone()),
                &PersistedQueriesPrewarmQueryPlanCache {
                    on_startup: false,
                    on_reload: true,
                },
            )
            .await;
            assert_eq!(requests.len(), 2);

            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                0,
                "source" = "cache"
            );
            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                2,
                "source" = "persisted_query"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warm_up_from_previous_cache() {
        async {
            let queries = [
                "{ me { username } }".to_string(),
                "{ topProducts { upc } }".to_string(),
                "{ me { reviews { body } } }".to_string(),
            ];
            let cache = prepopulated_cache(queries.iter()).await;

            let requests = super::queries_to_warm_up(
                Some(cache),
                None,
                None,
                &PersistedQueriesPrewarmQueryPlanCache::default(),
            )
            .await;
            assert_eq!(requests.len(), 1, "warm up 1/3rd of the queries by default");

            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                1,
                "source" = "cache"
            );
            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                0,
                "source" = "persisted_query"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warm_up_from_previous_cache_with_custom_max() {
        async {
            let queries = [
                "{ me { username } }".to_string(),
                "{ topProducts { upc } }".to_string(),
                "{ me { reviews { body } } }".to_string(),
            ];
            let cache = prepopulated_cache(queries.iter()).await;

            let requests = super::queries_to_warm_up(
                Some(cache),
                Some(2),
                None,
                &PersistedQueriesPrewarmQueryPlanCache::default(),
            )
            .await;
            assert_eq!(requests.len(), 2, "warm up the max # of queries from cache");

            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                2,
                "source" = "cache"
            );
            assert_counter!(
                "apollo.router.query_planning.warmup.operations.expected",
                0,
                "source" = "persisted_query"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warmup_query_parser_layer() {
        async {
            // The functionality of this layer is heavily intertwined with the CachingQueryPlanner,
            // so we are just asserting some simple things here (such as tower compatibility). The
            // really effective tests are integration tests.

            let (mock, mut handle) = tower_test::mock::pair::<CachingRequest, ()>();
            let driver = tokio::task::spawn(async move {
                let (request, responder) = handle.next_request().await.unwrap();
                request.context.extensions().with_lock(|lock| {
                    assert!(
                        lock.get::<ParsedDocument>().is_some(),
                        "should have inserted ParsedDocument"
                    );
                });
                responder.send_response(());
            });

            let configuration = Arc::new(Configuration::default());
            let schema = Arc::new(
                Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap(),
            );

            let query_parsing_service =
                crate::pipeline::build_query_parsing_service(schema, configuration);

            let mut service = ServiceBuilder::new()
                .layer(WarmupParseQueryLayer::new(query_parsing_service))
                .map_err(downcast_planning_mock_err)
                .service(mock);

            let _response: () = service
                .ready()
                .await
                .unwrap()
                .call(WarmupRequest {
                    query: "{ me { username } }".to_string(),
                    operation_name: None,
                    metadata: None,
                    plan_options: None,
                    source: WarmUpSource::Cache,
                })
                .await
                .unwrap();

            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "apollo.router.query_planning.warmup.operations",
                1,
                "outcome" = "success",
                "source" = "cache"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warmup_records_success_outcome() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<CachingRequest, ()>();
            let driver = tokio::task::spawn(async move {
                let (_request, responder) = handle.next_request().await.unwrap();
                responder.send_response(());
            });

            let configuration = Arc::new(Configuration::default());
            let schema = Arc::new(
                Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap(),
            );

            let query_parsing_service =
                crate::pipeline::build_query_parsing_service(schema, configuration);

            let mut service = ServiceBuilder::new()
                .layer(WarmupParseQueryLayer::new(query_parsing_service))
                .map_err(downcast_planning_mock_err)
                .service(mock);

            let _response: () = service
                .ready()
                .await
                .unwrap()
                .call(WarmupRequest {
                    query: "{ me { username } }".to_string(),
                    operation_name: None,
                    metadata: None,
                    plan_options: None,
                    source: WarmUpSource::PersistedQuery,
                })
                .await
                .unwrap();

            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "apollo.router.query_planning.warmup.operations",
                1,
                "outcome" = "success",
                "source" = "persisted_query"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warmup_records_parsing_backpressure_errors() {
        async {
            let (mock, handle) = tower_test::mock::pair::<CachingRequest, ()>();

            let (query_parsing_mock, mut query_parsing_handle) =
                tower_test::mock::pair::<query_parsing::Request, query_parsing::ParsedDocument>();
            let query_parsing_driver = tokio::task::spawn(async move {
                let (_request, responder) = query_parsing_handle.next_request().await.unwrap();
                responder.send_error(MaybeBackPressureError::TemporaryError(
                    crate::compute_job::ComputeBackPressureError,
                ) as query_parsing::ServiceError);
            });

            let query_parsing_service = ServiceBuilder::new()
                .map_err(downcast_parsing_mock_err)
                .service(query_parsing_mock)
                .boxed_clone();

            let mut service = ServiceBuilder::new()
                .layer(WarmupParseQueryLayer::new(query_parsing_service))
                .map_err(downcast_planning_mock_err)
                .service(mock);

            let result = service
                .ready()
                .await
                .unwrap()
                .call(WarmupRequest {
                    query: "{ me { username } }".to_string(),
                    operation_name: None,
                    metadata: None,
                    plan_options: None,
                    source: WarmUpSource::PersistedQuery,
                })
                .await;
            if !matches!(&result, Err(CacheResolverError::Backpressure(_))) {
                panic!("expected backpressure error, got {result:?}");
            }

            crate::plugin::test::assert_no_mock_calls(handle).await;
            crate::plugin::test::await_mock_driver(query_parsing_driver).await;

            assert_counter!(
                "apollo.router.query_planning.warmup.backpressure",
                1,
                "source" = "persisted_query",
                "phase" = "parse"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn warmup_records_planning_backpressure_errors() {
        async {
            let (mock, mut handle) = tower_test::mock::pair::<CachingRequest, ()>();
            let driver = tokio::task::spawn(async move {
                let (_request, responder) = handle.next_request().await.unwrap();
                responder.send_error(CacheResolverError::Backpressure(
                    crate::compute_job::ComputeBackPressureError,
                ));
            });

            let configuration = Arc::new(Configuration::default());
            let schema = Arc::new(
                Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap(),
            );

            let query_parsing_service =
                crate::pipeline::build_query_parsing_service(schema, configuration);

            let mut service = ServiceBuilder::new()
                .layer(WarmupParseQueryLayer::new(query_parsing_service))
                .map_err(downcast_planning_mock_err)
                .service(mock);

            let result = service
                .ready()
                .await
                .unwrap()
                .call(WarmupRequest {
                    query: "{ me { username } }".to_string(),
                    operation_name: None,
                    metadata: None,
                    plan_options: None,
                    source: WarmUpSource::PersistedQuery,
                })
                .await;
            if !matches!(&result, Err(CacheResolverError::Backpressure(_))) {
                panic!("expected backpressure error, got {result:?}");
            }

            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "apollo.router.query_planning.warmup.backpressure",
                1,
                "source" = "persisted_query",
                "phase" = "plan"
            );
        }
        .with_metrics()
        .await;
    }
}
