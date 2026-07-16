use std::sync::Arc;

use futures::future::BoxFuture;
use rand::seq::SliceRandom as _;
use tower::ServiceExt as _;

use crate::Context;
use crate::compute_job::ComputeJobType;
use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
use crate::error::CacheResolverError;
use crate::plugins::authorization;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::telemetry::utils::Timer;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::services::CachingRequest;
use crate::services::PlanOptions;
use crate::services::layers::query_analysis::QueryAnalysis;

pub(crate) type BoxCloneService =
    tower::util::BoxCloneService<WarmupRequest, (), CacheResolverError>;

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
}

/// Query parsing specifically for cache warmup.
///
/// XXX(@goto-bus-stop): Ideally we would do this with the ParseQueryLayer that
/// we also use for parsing inside the router service, but it requires a
/// different request/response type.
#[derive(Clone)]
pub(crate) struct WarmupParseQueryLayer {
    query_analysis: Arc<QueryAnalysis>,
}

impl WarmupParseQueryLayer {
    pub(crate) fn new(query_analysis: Arc<QueryAnalysis>) -> Self {
        Self { query_analysis }
    }
}

impl<S> tower::Layer<S> for WarmupParseQueryLayer {
    type Service = WarmupParseQueryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        WarmupParseQueryService {
            query_analysis: self.query_analysis.clone(),
            inner,
        }
    }
}

#[derive(Clone)]
pub(crate) struct WarmupParseQueryService<S> {
    query_analysis: Arc<QueryAnalysis>,
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
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: WarmupRequest) -> Self::Future {
        let query_analysis = self.query_analysis.clone();
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // XXX(@goto-bus-stop): we don't cache query parsing,
            // which would be a nice to have
            let result = query_analysis
                .parse_document(
                    &req.query,
                    req.operation_name.as_deref(),
                    ComputeJobType::QueryParsingWarmup,
                )
                .await;

            let document = match result {
                Ok(document) => document,
                Err(crate::compute_job::MaybeBackPressureError::PermanentError(err)) => {
                    return Err(CacheResolverError::RetrievalError(Arc::new(err.into())));
                }
                Err(crate::compute_job::MaybeBackPressureError::TemporaryError(err)) => {
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

            inner
                .call(CachingRequest {
                    query: req.query,
                    operation_name: req.operation_name,
                    context,
                })
                .await
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
            })
            .collect(),
        _ => Vec::new(),
    };
    persisted_query_keys.shuffle(&mut rand::rng());

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
        f64_histogram!(
            "apollo.router.query_planning.warmup.duration",
            "Time spent warming up the query planner queries in seconds",
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use crate::Configuration;
    use crate::cache::storage::CacheStorage;
    use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
    use crate::error::QueryPlannerError;
    use crate::query_planner::CachingQueryKey;
    use crate::query_planner::ConfigModeHash;
    use crate::query_planner::InMemoryQueryPlanCache;
    use crate::query_planner::QueryPlan;
    use crate::query_planner::warmup::WarmupParseQueryLayer;
    use crate::query_planner::warmup::WarmupRequest;
    use crate::services::CachingRequest;
    use crate::services::QueryPlannerContent;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::services::layers::query_analysis::QueryAnalysis;
    use crate::spec::Schema;
    use crate::spec::SchemaHash;

    /// Returns an in-memory cache with the given queries inside.
    async fn prepopulated_cache<Q: AsRef<str>>(
        queries: impl Iterator<Item = Q>,
    ) -> InMemoryQueryPlanCache {
        let cache =
            CacheStorage::new_in_memory(NonZeroUsize::new(100).unwrap(), "test").in_memory_cache();

        // Moot for these tests
        let schema_hash = SchemaHash::new("");

        fn empty_query_plan() -> Result<QueryPlannerContent, Arc<QueryPlannerError>> {
            Ok(QueryPlannerContent::Plan {
                plan: Arc::new(QueryPlan::fake_new(None, None)),
            })
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
    }

    #[tokio::test]
    async fn warm_up_pqs_on_reload() {
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
    }

    #[tokio::test]
    async fn warm_up_from_previous_cache() {
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
    }

    #[tokio::test]
    async fn warm_up_from_previous_cache_with_custom_max() {
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
    }

    #[tokio::test]
    async fn warmup_query_parser_layer() {
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

        let query_analysis = Arc::new(QueryAnalysis::new(schema, configuration).await);

        let mut service = ServiceBuilder::new()
            .layer(WarmupParseQueryLayer::new(query_analysis))
            .map_err(|err| {
                panic!(
                    "we have to cast the error because these services do not use BoxError: {err}"
                )
            })
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
            })
            .await
            .unwrap();

        crate::plugin::test::await_mock_driver(driver).await;
    }
}
