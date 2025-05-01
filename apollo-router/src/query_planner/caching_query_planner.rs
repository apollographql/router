use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::task;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use query_planner::QueryPlannerPlugin;
use rand::seq::SliceRandom;
use rand::thread_rng;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use crate::Configuration;
use crate::apollo_studio_interop::UsageReporting;
use crate::cache::DeduplicatingCache;
use crate::cache::EntryError;
use crate::cache::estimate_size;
use crate::cache::storage::InMemoryCache;
use crate::cache::storage::ValueType;
use crate::compute_job::ComputeBackPressureError;
use crate::compute_job::MaybeBackPressureError;
use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::telemetry::utils::Timer;
use crate::query_planner::QueryPlannerService;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::query_planner;
use crate::services::query_planner::PlanOptions;
use crate::spec::QueryHash;
use crate::spec::Schema;
use crate::spec::SchemaHash;
use crate::spec::SpecError;
use crate::test_harness::BlockQueryPlanningSignal;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn QueryPlannerPlugin>>;
pub(crate) type InMemoryCachePlanner =
    InMemoryCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>;
pub(crate) const APOLLO_OPERATION_ID: &str = "apollo::supergraph::operation_id";
pub(crate) const DEPRECATED_APOLLO_OPERATION_ID: &str = "apollo_operation_id";

/// Hashed value of query planner configuration for use in cache keys.
#[derive(Clone, Hash, PartialEq, Eq)]
// XXX(@goto-bus-stop): I think this probably should not be pub(crate), but right now all fields in
// the cache keys are pub(crate), which I'm not going to change at this time :)
pub(crate) struct ConfigModeHash(Vec<u8>);

impl std::fmt::Display for ConfigModeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

impl std::fmt::Debug for ConfigModeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ConfigModeHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner<T: Clone> {
    cache: Arc<
        DeduplicatingCache<
            CachingQueryKey,
            Result<QueryPlannerContent, Arc<QueryPlannerError>>,
            ComputeBackPressureError,
        >,
    >,
    delegate: T,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    plugins: Arc<Plugins>,
    enable_authorization_directives: bool,
    config_mode_hash: Arc<ConfigModeHash>,
}

fn init_query_plan_from_redis(
    subgraph_schemas: &SubgraphSchemas,
    cache_entry: &mut Result<QueryPlannerContent, Arc<QueryPlannerError>>,
) -> Result<(), String> {
    if let Ok(QueryPlannerContent::Plan { plan }) = cache_entry {
        // Arc freshly deserialized from Redis should be unique, so this doesn’t clone:
        let plan = Arc::make_mut(plan);
        let root = Arc::make_mut(&mut plan.root);
        root.init_parsed_operations(subgraph_schemas)
            .map_err(|e| format!("Invalid subgraph operation: {e}"))?
    }
    Ok(())
}

impl<T: Clone + 'static> CachingQueryPlanner<T>
where
    T: tower::Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = MaybeBackPressureError<QueryPlannerError>,
        > + Send,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) async fn new(
        delegate: T,
        schema: Arc<Schema>,
        subgraph_schemas: Arc<SubgraphSchemas>,
        configuration: &Configuration,
        plugins: Plugins,
    ) -> Result<CachingQueryPlanner<T>, BoxError> {
        let cache = Arc::new(
            DeduplicatingCache::from_configuration(
                &configuration.supergraph.query_planning.cache.clone().into(),
                "query planner",
            )
            .await?,
        );

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(configuration, &schema).unwrap_or(false);

        let mut hasher = StructHasher::new();
        configuration.rust_query_planner_config().hash(&mut hasher);
        let config_mode_hash = Arc::new(ConfigModeHash(hasher.finalize()));

        Ok(Self {
            cache,
            delegate,
            schema,
            subgraph_schemas,
            plugins: Arc::new(plugins),
            enable_authorization_directives,
            config_mode_hash,
        })
    }

    pub(crate) fn previous_cache(&self) -> InMemoryCachePlanner {
        self.cache.in_memory_cache()
    }

    pub(crate) async fn warm_up(
        &mut self,
        query_analysis: &QueryAnalysisLayer,
        persisted_query_layer: &PersistedQueryLayer,
        previous_cache: Option<InMemoryCachePlanner>,
        count: Option<usize>,
        experimental_reuse_query_plans: bool,
        experimental_pql_prewarm: &PersistedQueriesPrewarmQueryPlanCache,
    ) {
        let _timer = Timer::new(|duration| {
            f64_histogram!(
                "apollo.router.query_planning.warmup.duration",
                "Time spent warming up the query planner queries in seconds",
                duration.as_secs_f64()
            );
        });

        let mut service = ServiceBuilder::new().service(
            self.plugins
                .iter()
                .rev()
                .fold(self.delegate.clone().boxed(), |acc, (_, e)| {
                    e.query_planner_service(acc)
                }),
        );

        let mut cache_keys = match previous_cache {
            Some(ref previous_cache) => {
                let cache = previous_cache.lock().await;

                let count = count.unwrap_or(cache.len() / 3);

                cache
                    .iter()
                    .map(
                        |(
                            CachingQueryKey {
                                query,
                                operation,
                                hash,
                                metadata,
                                plan_options,
                                config_mode_hash: _,
                                schema_id: _,
                            },
                            _,
                        )| WarmUpCachingQueryKey {
                            query: query.clone(),
                            operation_name: operation.clone(),
                            hash: Some(hash.clone()),
                            metadata: metadata.clone(),
                            plan_options: plan_options.clone(),
                            config_mode_hash: self.config_mode_hash.clone(),
                        },
                    )
                    .take(count)
                    .collect::<Vec<_>>()
            }
            None => Vec::new(),
        };

        cache_keys.shuffle(&mut thread_rng());

        let should_warm_with_pqs = (experimental_pql_prewarm.on_startup
            && previous_cache.is_none())
            || (experimental_pql_prewarm.on_reload && previous_cache.is_some());
        let persisted_queries_operations = persisted_query_layer.all_operations();

        let capacity = if should_warm_with_pqs {
            cache_keys.len()
                + persisted_queries_operations
                    .as_ref()
                    .map(|ops| ops.len())
                    .unwrap_or(0)
        } else {
            cache_keys.len()
        };

        if capacity > 0 {
            tracing::info!(
                "warming up the query plan cache with {} queries, this might take a while",
                capacity
            );
        }

        // persisted queries are added first because they should get a lower priority in the LRU cache,
        // since a lot of them may be there to support old clients
        let mut all_cache_keys: Vec<WarmUpCachingQueryKey> = Vec::with_capacity(capacity);
        if should_warm_with_pqs {
            if let Some(queries) = persisted_queries_operations {
                for query in queries {
                    all_cache_keys.push(WarmUpCachingQueryKey {
                        query,
                        operation_name: None,
                        hash: None,
                        metadata: CacheKeyMetadata::default(),
                        plan_options: PlanOptions::default(),
                        config_mode_hash: self.config_mode_hash.clone(),
                    });
                }
            }
        }

        all_cache_keys.shuffle(&mut thread_rng());

        all_cache_keys.extend(cache_keys.into_iter());

        let mut count = 0usize;
        let mut reused = 0usize;
        'all_cache_keys_loop: for WarmUpCachingQueryKey {
            query,
            operation_name,
            hash,
            metadata,
            plan_options,
            config_mode_hash: _,
        } in all_cache_keys
        {
            let doc = match query_analysis
                .parse_document(&query, operation_name.as_deref())
                .await
            {
                Ok(doc) => doc,
                Err(_) => continue,
            };

            let caching_key = CachingQueryKey {
                query: query.clone(),
                operation: operation_name.clone(),
                hash: doc.hash.clone(),
                schema_id: self.schema.schema_id.clone(),
                metadata,
                plan_options,
                config_mode_hash: self.config_mode_hash.clone(),
            };

            if experimental_reuse_query_plans {
                // check if prewarming via seeing if the previous cache exists (aka a reloaded router); if reloading, try to reuse the
                if let Some(ref previous_cache) = previous_cache {
                    // if the query hash did not change with the schema update, we can reuse the previously cached entry
                    if let Some(hash) = hash {
                        if hash == doc.hash {
                            if let Some(entry) =
                                { previous_cache.lock().await.get(&caching_key).cloned() }
                            {
                                self.cache.insert_in_memory(caching_key, entry).await;
                                reused += 1;
                                continue;
                            }
                        }
                    }
                }
            };

            let entry = self
                .cache
                .get(&caching_key, |v| {
                    init_query_plan_from_redis(&self.subgraph_schemas, v)
                })
                .await;
            if entry.is_first() {
                let doc = loop {
                    match query_analysis
                        .parse_document(&query, operation_name.as_deref())
                        .await
                    {
                        Ok(doc) => break doc,
                        Err(MaybeBackPressureError::PermanentError(error)) => {
                            let e = Arc::new(QueryPlannerError::SpecError(error));
                            tokio::spawn(async move {
                                entry.insert(Err(e)).await;
                            });
                            continue 'all_cache_keys_loop;
                        }
                        Err(MaybeBackPressureError::TemporaryError(ComputeBackPressureError)) => {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            // try again
                        }
                    }
                };

                loop {
                    let request = QueryPlannerRequest::builder()
                        .query(query.clone())
                        .and_operation_name(operation_name.clone())
                        .document(doc.clone())
                        .metadata(caching_key.metadata.clone())
                        .plan_options(caching_key.plan_options.clone())
                        .build();
                    let res = match service.ready().await {
                        Ok(service) => service.call(request).await,
                        Err(_) => break 'all_cache_keys_loop,
                    };

                    match res {
                        Ok(QueryPlannerResponse { content, .. }) => {
                            if let Some(content) = content.clone() {
                                count += 1;
                                tokio::spawn(async move {
                                    entry.insert(Ok(content.clone())).await;
                                });
                            }
                            break;
                        }
                        Err(MaybeBackPressureError::PermanentError(error)) => {
                            count += 1;
                            let e = Arc::new(error);
                            tokio::spawn(async move {
                                entry.insert(Err(e)).await;
                            });
                            break;
                        }
                        Err(MaybeBackPressureError::TemporaryError(ComputeBackPressureError)) => {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            // try again
                        }
                    }
                }
            }
        }

        tracing::debug!(
            "warmed up the query planner cache with {count} queries planned and {reused} queries reused"
        );
    }
}

impl CachingQueryPlanner<QueryPlannerService> {
    pub(crate) fn subgraph_schemas(&self) -> Arc<SubgraphSchemas> {
        self.delegate.subgraph_schemas()
    }

    pub(crate) fn activate(&self) {
        self.cache.activate();
        self.delegate.activate();
    }
}

impl<T: Clone + Send + 'static> tower::Service<query_planner::CachingRequest>
    for CachingQueryPlanner<T>
where
    T: tower::Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = MaybeBackPressureError<QueryPlannerError>,
        >,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    type Response = QueryPlannerResponse;
    type Error = CacheResolverError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: query_planner::CachingRequest) -> Self::Future {
        let qp = self.clone();
        Box::pin(async move {
            let context = request.context.clone();
            qp.plan(request).await.inspect(|_response| {
                if let Some(usage_reporting) = context
                    .extensions()
                    .with_lock(|lock| lock.get::<Arc<UsageReporting>>().cloned())
                {
                    let _ = context.insert(APOLLO_OPERATION_ID, usage_reporting.get_operation_id());
                    let _ = context.insert(
                        "apollo_operation_signature",
                        usage_reporting.get_stats_report_key(),
                    );
                }
            })
        })
    }
}

impl<T> CachingQueryPlanner<T>
where
    T: tower::Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = MaybeBackPressureError<QueryPlannerError>,
        > + Clone
        + Send
        + 'static,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    async fn plan(
        mut self,
        request: query_planner::CachingRequest,
    ) -> Result<<T as tower::Service<QueryPlannerRequest>>::Response, CacheResolverError> {
        if self.enable_authorization_directives {
            AuthorizationPlugin::update_cache_key(&request.context);
        }

        let plan_options = PlanOptions {
            override_conditions: request
                .context
                .get(LABELS_TO_OVERRIDE_KEY)
                .unwrap_or_default()
                .unwrap_or_default(),
        };

        let doc = match request
            .context
            .extensions()
            .with_lock(|lock| lock.get::<ParsedDocument>().cloned())
        {
            None => {
                return Err(CacheResolverError::RetrievalError(Arc::new(
                    // TODO: dedicated error variant?
                    QueryPlannerError::SpecError(SpecError::TransformError(
                        "missing parsed document".to_string(),
                    )),
                )));
            }
            Some(d) => d.clone(),
        };

        let metadata = request
            .context
            .extensions()
            .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned())
            .unwrap_or_default();

        let caching_key = CachingQueryKey {
            query: request.query.clone(),
            operation: request.operation_name.to_owned(),
            hash: doc.hash.clone(),
            schema_id: self.schema.schema_id.clone(),
            metadata,
            plan_options,
            config_mode_hash: self.config_mode_hash.clone(),
        };

        let context = request.context.clone();
        let entry = self
            .cache
            .get(&caching_key, |v| {
                init_query_plan_from_redis(&self.subgraph_schemas, v)
            })
            .await;
        if entry.is_first() {
            let query_planner::CachingRequest {
                query,
                operation_name,
                context,
            } = request;

            let test_block_planning_progress = if cfg!(test) {
                context
                    .extensions()
                    .with_lock(|locked| locked.get::<BlockQueryPlanningSignal>().cloned())
            } else {
                // Don't needlessly lock context extensions
                None
            };
            let request = QueryPlannerRequest::builder()
                .query(query)
                .and_operation_name(operation_name)
                .document(doc)
                .metadata(caching_key.metadata)
                .plan_options(caching_key.plan_options)
                .and_test_block_planning_progress(test_block_planning_progress)
                .build();

            let qp_task = tokio::spawn(
                async move {
                    let service = match self.delegate.ready().await {
                        Ok(service) => service,
                        Err(MaybeBackPressureError::PermanentError(error)) => {
                            let e = Arc::new(error);
                            let err = e.clone();
                            tokio::spawn(async move {
                                entry.insert(Err(err)).await;
                            });
                            return Err(CacheResolverError::RetrievalError(e));
                        }
                        Err(MaybeBackPressureError::TemporaryError(error)) => {
                            let err = error.clone();
                            tokio::spawn(async move {
                                // Temporary errors are never cached
                                entry.send(Err(err)).await;
                            });
                            return Err(CacheResolverError::Backpressure(error));
                        }
                    };

                    let res = service.call(request).await;

                    match res {
                        Ok(QueryPlannerResponse { content, errors }) => {
                            if let Some(content) = content.clone() {
                                let can_cache = match &content {
                                    // Already cached in an introspection-specific, small-size,
                                    // in-memory-only cache.
                                    QueryPlannerContent::CachedIntrospectionResponse { .. } => {
                                        false
                                    }
                                    _ => true,
                                };

                                if can_cache {
                                    tokio::spawn(async move {
                                        entry.insert(Ok(content)).await;
                                    });
                                } else {
                                    tokio::spawn(async move {
                                        entry.send(Ok(Ok(content))).await;
                                    });
                                }
                            }

                            // This will be overridden by the Rust usage reporting implementation
                            if let Some(QueryPlannerContent::Plan { plan, .. }) = &content {
                                context.extensions().with_lock(|lock| {
                                    lock.insert::<Arc<UsageReporting>>(plan.usage_reporting.clone())
                                });
                            }
                            Ok(QueryPlannerResponse { content, errors })
                        }
                        Err(MaybeBackPressureError::PermanentError(error)) => {
                            let e = Arc::new(error);
                            let err = e.clone();
                            tokio::spawn(async move {
                                entry.insert(Err(err)).await;
                            });
                            if let Some(usage_reporting) = e.usage_reporting() {
                                context.extensions().with_lock(|lock| {
                                    lock.insert::<Arc<UsageReporting>>(Arc::new(usage_reporting));
                                });
                            }
                            Err(CacheResolverError::RetrievalError(e))
                        }
                        Err(MaybeBackPressureError::TemporaryError(error)) => {
                            let err = error.clone();
                            tokio::spawn(async move {
                                // Temporary errors are never cached
                                entry.send(Err(err)).await;
                            });
                            Err(CacheResolverError::Backpressure(error))
                        }
                    }
                }
                .in_current_span(),
            );

            let _abort_guard = scopeguard::guard(qp_task.abort_handle(), |abort_handle| {
                // Abort is a no-op if the task has already completed.
                abort_handle.abort();
            });

            // Receive the result of planning. If the request is cancelled, we may be dropped at
            // this await point. The `abort_guard` then propagates that to query planning.
            qp_task.await.map_err(|e| {
                CacheResolverError::RetrievalError(Arc::new(QueryPlannerError::JoinError(
                    e.to_string(),
                )))
            })?
        } else {
            let res = entry.get().await.map_err(|e| match e {
                EntryError::IsFirst | // IsFirst should be unreachable
                EntryError::RecvError => QueryPlannerError::UnhandledPlannerResult.into(),
                EntryError::UncachedError(e) => CacheResolverError::Backpressure(e),
            })?;

            match res {
                Ok(content) => {
                    if let QueryPlannerContent::Plan { plan, .. } = &content {
                        context.extensions().with_lock(|lock| {
                            lock.insert::<Arc<UsageReporting>>(plan.usage_reporting.clone())
                        });
                    }

                    Ok(QueryPlannerResponse::builder().content(content).build())
                }
                Err(error) => {
                    if let Some(usage_reporting) = error.usage_reporting() {
                        context.extensions().with_lock(|lock| {
                            lock.insert::<Arc<UsageReporting>>(Arc::new(usage_reporting));
                        });
                    }

                    Err(CacheResolverError::RetrievalError(error))
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CachingQueryKey {
    pub(crate) query: String,
    pub(crate) operation: Option<String>,
    pub(crate) hash: Arc<QueryHash>,
    // XXX(@goto-bus-stop): It's probably correct to remove this, since having it here is
    // misleading. The schema ID is *not* used in the Redis cache, but it's okay because the QueryHash
    // is schema-aware.
    pub(crate) schema_id: SchemaHash,
    pub(crate) metadata: CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
    pub(crate) config_mode_hash: Arc<ConfigModeHash>,
}

const ROUTER_VERSION: &str = env!("CARGO_PKG_VERSION");

impl std::fmt::Display for CachingQueryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut hasher = Sha256::new();
        hasher.update(self.operation.as_deref().unwrap_or("-"));
        let operation = hex::encode(hasher.finalize());

        let mut hasher = StructHasher::new();
        "^metadata".hash(&mut hasher);
        self.metadata.hash(&mut hasher);
        "^plan_options".hash(&mut hasher);
        self.plan_options.hash(&mut hasher);
        "^config_mode".hash(&mut hasher);
        self.config_mode_hash.hash(&mut hasher);
        let metadata = hex::encode(hasher.finalize());

        write!(
            f,
            "plan:router:{}:{}:opname:{}:metadata:{}",
            ROUTER_VERSION, self.hash, operation, metadata,
        )
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct WarmUpCachingQueryKey {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) hash: Option<Arc<QueryHash>>,
    pub(crate) metadata: CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
    pub(crate) config_mode_hash: Arc<ConfigModeHash>,
}

struct StructHasher {
    hasher: Sha256,
}

impl StructHasher {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }
    fn finalize(self) -> Vec<u8> {
        self.hasher.finalize().as_slice().into()
    }
}

impl Hasher for StructHasher {
    fn finish(&self) -> u64 {
        unreachable!()
    }

    fn write(&mut self, bytes: &[u8]) {
        self.hasher.update(&[0xFF][..]);
        self.hasher.update(bytes);
    }
}

impl ValueType for Result<QueryPlannerContent, Arc<QueryPlannerError>> {
    fn estimated_size(&self) -> Option<usize> {
        match self {
            Ok(QueryPlannerContent::Plan { plan }) => Some(plan.estimated_size()),
            Ok(QueryPlannerContent::Response { response })
            | Ok(QueryPlannerContent::CachedIntrospectionResponse { response }) => {
                Some(estimate_size(response))
            }
            Ok(QueryPlannerContent::IntrospectionDisabled) => None,
            Err(e) => Some(estimate_size(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mockall::mock;
    use serde_json_bytes::json;
    use test_log::test;
    use tower::Service;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::apollo_studio_interop::UsageReporting;
    use crate::json_ext::Object;
    use crate::query_planner::QueryPlan;
    use crate::spec::Query;
    use crate::spec::Schema;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_call(
                &self,
                key: QueryPlannerRequest,
            ) -> Result<QueryPlannerResponse, MaybeBackPressureError<QueryPlannerError>>;
        }

        impl Clone for MyQueryPlanner {
            fn clone(&self) -> MockMyQueryPlanner;
        }
    }

    impl Service<QueryPlannerRequest> for MockMyQueryPlanner {
        type Response = QueryPlannerResponse;

        type Error = MaybeBackPressureError<QueryPlannerError>;

        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut task::Context<'_>,
        ) -> task::Poll<Result<(), Self::Error>> {
            task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
            let res = self.sync_call(req);
            Box::pin(async move { res })
        }
    }

    #[test(tokio::test)]
    async fn test_plan() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate.expect_clone().returning(|| {
            let mut planner = MockMyQueryPlanner::new();
            planner
                .expect_sync_call()
                .times(0..2)
                .returning(|_| Err(QueryPlannerError::UnhandledPlannerResult.into()));
            planner
        });

        let configuration = Arc::new(crate::Configuration::default());
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            delegate,
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        for _ in 0..5 {
            assert!(
                planner
                    .call(
                        query_planner::CachingRequest::fake_builder()
                            .configuration(configuration.clone())
                            .schema(schema.clone())
                            .query("query Me { me { username } }")
                            .build()
                    )
                    .await
                    .is_err()
            );
        }

        assert!(
            planner
                .call(
                    query_planner::CachingRequest::fake_builder()
                        .configuration(configuration.clone())
                        .schema(schema.clone())
                        .query("query Me { me { name { first } } }")
                        .build()
                )
                .await
                .is_err()
        );
    }

    macro_rules! test_query_plan {
        () => {
            include_str!("testdata/query_plan.json")
        };
    }

    #[test(tokio::test)]
    async fn test_usage_reporting() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate.expect_clone().returning(|| {
            let mut planner = MockMyQueryPlanner::new();
            planner.expect_sync_call().times(0..2).returning(|_| {
                let query_plan: QueryPlan = QueryPlan {
                    formatted_query_plan: Default::default(),
                    root: serde_json::from_str(test_query_plan!()).unwrap(),
                    usage_reporting: UsageReporting::Error("this is a test report key".to_string())
                        .into(),
                    query: Arc::new(Query::empty_for_tests()),
                    query_metrics: Default::default(),
                    estimated_size: Default::default(),
                };
                let qp_content = QueryPlannerContent::Plan {
                    plan: Arc::new(query_plan),
                };

                Ok(QueryPlannerResponse::builder().content(qp_content).build())
            });
            planner
        });

        let configuration = Configuration::default();

        let schema =
            Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap();

        let doc = Query::parse_document(
            "query Me { me { username } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let mut planner = CachingQueryPlanner::new(
            delegate,
            Arc::new(schema),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        for _ in 0..5 {
            let _ = planner
                .call(
                    query_planner::CachingRequest::builder()
                        .query("query Me { me { username } }")
                        .context(context.clone())
                        .build(),
                )
                .await
                .unwrap();
            assert!(
                context
                    .extensions()
                    .with_lock(|lock| lock.contains_key::<Arc<UsageReporting>>())
            );
        }
    }

    #[test(tokio::test)]
    async fn test_introspection_cache() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate
            .expect_clone()
            // This is the main point of the test: if introspection queries are not cached, then the delegate
            // will be called twice when we send the same request twice
            .times(2)
            .returning(|| {
                let mut planner = MockMyQueryPlanner::new();
                planner.expect_sync_call().returning(|_| {
                    let qp_content = QueryPlannerContent::CachedIntrospectionResponse {
                        response: Box::new(
                            crate::graphql::Response::builder()
                                .data(Object::new())
                                .build(),
                        ),
                    };

                    Ok(QueryPlannerResponse::builder().content(qp_content).build())
                });
                planner
            });

        let configuration = Arc::new(Default::default());
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            delegate,
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        assert!(
            planner
                .call(
                    query_planner::CachingRequest::fake_builder()
                        .configuration(configuration.clone())
                        .schema(schema.clone())
                        .query("{ __schema { types { name } } }")
                        .build()
                )
                .await
                .is_ok()
        );

        assert!(
            planner
                .call(
                    query_planner::CachingRequest::fake_builder()
                        .configuration(configuration.clone())
                        .schema(schema.clone())
                        .query("{ __schema { types { name } } }")
                        .build()
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_cancellation_propagation() {
        #[derive(Debug, PartialEq, Eq)]
        enum Status {
            NotStarted,
            Planning,
            Cancelled,
        }

        #[derive(Clone)]
        struct NeverCompletesQueryPlanner {
            status: tokio::sync::mpsc::Sender<Status>,
        }

        impl Service<QueryPlannerRequest> for NeverCompletesQueryPlanner {
            type Response = QueryPlannerResponse;

            type Error = MaybeBackPressureError<QueryPlannerError>;

            type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

            fn poll_ready(
                &mut self,
                _cx: &mut task::Context<'_>,
            ) -> task::Poll<Result<(), Self::Error>> {
                task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, _req: QueryPlannerRequest) -> Self::Future {
                let status = self.status.clone();
                Box::pin(async move {
                    let _guard = scopeguard::guard(status.clone(), |status| {
                        // Executed on Drop, thus needs to be sync
                        tokio::spawn(async move {
                            status
                                .send(Status::Cancelled)
                                .await
                                .expect("test is wrong if the channel is closed");
                        });
                    });

                    status
                        .send(Status::Planning)
                        .await
                        .expect("test is wrong if the channel is closed");
                    Ok(std::future::pending::<QueryPlannerResponse>().await)
                })
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        tx.send(Status::NotStarted)
            .await
            .expect("test is wrong if the channel is closed");

        let delegate = NeverCompletesQueryPlanner { status: tx };
        let configuration = Arc::new(crate::Configuration::default());
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            delegate,
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let fut = planner.call(
            query_planner::CachingRequest::fake_builder()
                .configuration(configuration)
                .schema(schema)
                .query("query Me { me { name { first } } }")
                .operation_name("Me")
                .build(),
        );

        let handle = tokio::task::spawn(fut);

        const SECOND: Duration = Duration::from_secs(1);

        assert_eq!(
            tokio::time::timeout(SECOND, rx.recv()).await.unwrap(),
            Some(Status::NotStarted),
        );

        assert_eq!(
            tokio::time::timeout(SECOND, rx.recv()).await.unwrap(),
            Some(Status::Planning),
            "should have started planning"
        );

        handle.abort();

        assert_eq!(
            tokio::time::timeout(SECOND, rx.recv()).await.unwrap(),
            Some(Status::Cancelled),
            "should have cancelled planning"
        );
    }

    // Expect that if we call the CQP twice, the second call will return cached data
    #[test(tokio::test)]
    async fn test_cache_works() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate.expect_clone().times(2).returning(|| {
            let mut planner = MockMyQueryPlanner::new();
            planner
                .expect_sync_call()
                // Don't allow the delegate to be called more than once
                .times(1)
                .returning(|_| {
                    let qp_content = QueryPlannerContent::CachedIntrospectionResponse {
                        response: Box::new(
                            crate::graphql::Response::builder()
                                .data(json!(r#"{"data":{"me":{"name":"Ada Lovelace"}}}%"#))
                                .build(),
                        ),
                    };

                    Ok(QueryPlannerResponse::builder().content(qp_content).build())
                });
            planner
        });

        let configuration = Default::default();
        let schema = include_str!("../testdata/starstuff@current.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            delegate,
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query ExampleQuery { me { name } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();
        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        let _ = planner
            .call(query_planner::CachingRequest::new(
                "query ExampleQuery {
                  me {
                    name
                  }
                }"
                .to_string(),
                None,
                context.clone(),
            ))
            .await
            .unwrap();

        let _ = planner
            .call(query_planner::CachingRequest::new(
                "query ExampleQuery {
                  me {
                    name
                  }
                }"
                .to_string(),
                None,
                context.clone(),
            ))
            .await
            .unwrap();
    }

    #[test(tokio::test)]
    async fn test_temporary_errors_arent_cached() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate
            .expect_clone()
            // We're calling the caching QP twice, so we expect the delegate to be cloned twice
            .times(2)
            .returning(|| {
                // Expect each clone to be called once since the return value isn't cached
                let mut planner = MockMyQueryPlanner::new();
                planner.expect_sync_call().times(1).returning(|_| {
                    Err(MaybeBackPressureError::TemporaryError(
                        ComputeBackPressureError,
                    ))
                });
                planner
            });

        let configuration = Default::default();
        let schema = include_str!("../testdata/starstuff@current.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            delegate,
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query ExampleQuery { me { name } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        let r = planner
            .call(query_planner::CachingRequest::new(
                "query ExampleQuery {
                  me {
                    name
                  }
                }"
                .to_string(),
                None,
                context.clone(),
            ))
            .await;

        let r2 = planner
            .call(query_planner::CachingRequest::new(
                "query ExampleQuery {
                  me {
                    name
                  }
                }"
                .to_string(),
                None,
                context.clone(),
            ))
            .await;

        if let (Err(e), Err(e2)) = (r, r2) {
            assert_eq!(e.to_string(), e2.to_string());
        } else {
            panic!("Expected both calls to return same error");
        }
    }
}
