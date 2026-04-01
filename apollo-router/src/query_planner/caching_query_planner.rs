use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::task;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use query_planner::QueryPlannerPlugin;
use rand::seq::SliceRandom;
use sha2::Digest;
use sha2::Sha256;
use tokio_util::time::FutureExt;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use crate::Configuration;
use crate::allocator::WithMemoryTracking;
use crate::apollo_studio_interop::UsageReporting;
use crate::cache::DeduplicatingCache;
use crate::cache::EntryError;
use crate::cache::estimate_size;
use crate::cache::storage::InMemoryCache;
use crate::cache::storage::ValueType;
use crate::compute_job::ComputeBackPressureError;
use crate::compute_job::ComputeJobType;
use crate::compute_job::MaybeBackPressureError;
use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
use crate::configuration::cooperative_cancellation::CooperativeCancellation;
use crate::configuration::mode::Mode;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::layers::ServiceExt as _;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::limits;
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

#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Outcome {
    None = 0,
    Timeout = 1,
    Cancelled = 2,
    Success = 3,
    Error = 4,
    Backpressure = 5,
    BatchingError = 6,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::None => write!(f, "none"),
            Outcome::Timeout => write!(f, "timeout"),
            Outcome::Cancelled => write!(f, "cancelled"),
            Outcome::Success => write!(f, "success"),
            Outcome::Error => write!(f, "error"),
            Outcome::Backpressure => write!(f, "backpressure"),
            Outcome::BatchingError => write!(f, "batching_error"),
        }
    }
}

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn QueryPlannerPlugin>>;
pub(crate) type InMemoryCachePlanner =
    InMemoryCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>;
pub(crate) const APOLLO_OPERATION_ID: &str = "apollo::supergraph::operation_id";

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
    cooperative_cancellation: CooperativeCancellation,
    config_limits: limits::Config,
}

fn init_query_plan_from_redis(
    subgraph_schemas: &SubgraphSchemas,
    cache_entry: &mut Result<QueryPlannerContent, Arc<QueryPlannerError>>,
) -> Result<(), String> {
    if let Ok(QueryPlannerContent::Plan { plan }) = cache_entry {
        // Arc freshly deserialized from Redis should be unique, so this doesn't clone:
        let plan = Arc::make_mut(plan);
        let root = Arc::make_mut(&mut plan.root);
        root.init_parsed_operations(subgraph_schemas)
            .map_err(|e| format!("Invalid subgraph operation: {e}"))?
    }
    Ok(())
}

impl<T: Clone + Sync + 'static> CachingQueryPlanner<T>
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
        let cooperative_cancellation = configuration
            .supergraph
            .query_planning
            .experimental_cooperative_cancellation
            .clone();

        Ok(Self {
            cache,
            delegate,
            schema,
            subgraph_schemas,
            plugins: Arc::new(plugins),
            enable_authorization_directives,
            cooperative_cancellation,
            config_mode_hash,
            config_limits: configuration.limits.clone(),
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
                .fold(self.delegate.clone().boxed_clone_sync(), |acc, (_, e)| {
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

        cache_keys.shuffle(&mut rand::rng());

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
        if should_warm_with_pqs && let Some(queries) = persisted_queries_operations {
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

        all_cache_keys.shuffle(&mut rand::rng());

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
            // NB: warmup tasks have a low priority so that real requests are prioritized
            let doc = loop {
                match query_analysis
                    .parse_document(
                        &query,
                        operation_name.as_deref(),
                        ComputeJobType::QueryParsingWarmup,
                    )
                    .await
                {
                    Ok(doc) => break doc,
                    Err(MaybeBackPressureError::PermanentError(_)) => {
                        continue 'all_cache_keys_loop;
                    }
                    Err(MaybeBackPressureError::TemporaryError(ComputeBackPressureError)) => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        // try again
                    }
                }
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
                    if let Some(hash) = hash
                        && hash == doc.hash
                        && let Some(entry) =
                            { previous_cache.lock().await.get(&caching_key).cloned() }
                    {
                        self.cache.insert_in_memory(caching_key, entry).await;
                        reused += 1;
                        continue;
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
                loop {
                    let request = QueryPlannerRequest {
                        query: query.clone(),
                        operation_name: operation_name.clone(),
                        document: doc.clone(),
                        metadata: caching_key.metadata.clone(),
                        plan_options: caching_key.plan_options.clone(),
                        compute_job_type: ComputeJobType::QueryPlanningWarmup,
                        variables: Default::default(),
                    };
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

impl<T: Clone + Send + 'static> Service<query_planner::CachingRequest> for CachingQueryPlanner<T>
where
    T: Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = MaybeBackPressureError<QueryPlannerError>,
        >,
    <T as Service<QueryPlannerRequest>>::Future: Send,
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

const OUTCOME: &str = "outcome";

fn record_outcome_if_none(outcome_recorded: &AtomicU8, outcome: Outcome) -> bool {
    if outcome_recorded
        .compare_exchange(
            Outcome::None as u8,
            outcome as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
    {
        tracing::Span::current().record(OUTCOME, outcome.to_string());
        true
    } else {
        false
    }
}

impl<T> CachingQueryPlanner<T>
where
    T: Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = MaybeBackPressureError<QueryPlannerError>,
        > + Clone
        + Send
        + 'static,
    <T as Service<QueryPlannerRequest>>::Future: Send,
{
    async fn plan(
        mut self,
        request: query_planner::CachingRequest,
    ) -> Result<<T as Service<QueryPlannerRequest>>::Response, CacheResolverError> {
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
                variables,
            } = request;

            let request = QueryPlannerRequest::builder()
                .query(&query)
                .and_operation_name(operation_name)
                .document(doc)
                .metadata(caching_key.metadata)
                .plan_options(caching_key.plan_options)
                .compute_job_type(ComputeJobType::QueryPlanning)
                .variables(variables)
                .build();

            let planning_task = async move {
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
                                QueryPlannerContent::CachedIntrospectionResponse { .. } => false,
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
            .with_memory_tracking("planning_task")
            .in_current_span();

            fn convert_join_error(e: impl std::fmt::Display) -> CacheResolverError {
                CacheResolverError::RetrievalError(Arc::new(QueryPlannerError::JoinError(
                    e.to_string(),
                )))
            }

            let outcome_recorded = Arc::new(AtomicU8::new(Outcome::None as u8));
            // When cooperative cancellation is enabled, we want to cancel the query planner
            // task if the request is canceled.
            if self.cooperative_cancellation.is_enabled() {
                let outcome_recorded_for_abort = outcome_recorded.clone();
                let outcome_recorded_for_memory_limit = outcome_recorded.clone();
                let outcome_recorded_for_timeout = outcome_recorded.clone();

                match self.cooperative_cancellation.mode() {
                    Mode::Enforce => {
                        let cancelled = Arc::new(AtomicBool::new(false));
                        let exceeded_memory_limit = Arc::new(AtomicBool::new(false));
                        let cancellable_planning_task =
                            crate::compute_job::CANCEL_JOB.scope(Some(cancelled.clone()), planning_task);

                        let exceeded_memory_limit_setter = exceeded_memory_limit.clone();
                        let task = if let Some(memory_limit) = self.cooperative_cancellation.memory_limit() {
                            if let Some(stats) = crate::allocator::current() {
                                let memory_limit_bytes = memory_limit.as_u64() as usize;
                                let task = tokio::task::spawn(cancellable_planning_task);
                                let abort_handle = task.abort_handle();
                                stats.set_allocation_limit(memory_limit_bytes, Box::new(move |_bytes_allocated| {
                                    exceeded_memory_limit_setter.store(true, Ordering::Relaxed);
                                    abort_handle.abort();
                                        log::warn!("memory limit exceeded planning query: {}", &query);
                                    }));
                                task
                            } else {
                                log::error!("memory limit cooperative cancellation is set but no stats are available");
                                tokio::task::spawn(cancellable_planning_task)
                            }
                        } else {
                            tokio::task::spawn(cancellable_planning_task)
                        };

                        let _abort_guard =
                            scopeguard::guard(task.abort_handle(), move |abort_handle| {
                                if record_outcome_if_none(&outcome_recorded_for_abort, Outcome::Cancelled)
                                {
                                    cancelled.store(true, Ordering::Relaxed);
                                    abort_handle.abort();
                                }
                            });

                        if let Some(timeout) = self.cooperative_cancellation.timeout() {
                            match task.timeout(timeout).await {
                                Ok(result) => match result {
                                    Err(e) if exceeded_memory_limit.load(Ordering::Relaxed) => {
                                        record_outcome_if_none(
                                            &outcome_recorded_for_memory_limit,
                                            Outcome::Cancelled,
                                        );
                                        Err(CacheResolverError::RetrievalError(Arc::new(
                                        QueryPlannerError::MemoryLimitExceeded(e.to_string()),
                                        )))?
                                    },
                                    result => result,
                                },
                                Err(e) => {
                                    record_outcome_if_none(
                                        &outcome_recorded_for_timeout,
                                        Outcome::Timeout,
                                    );
                                    Err(CacheResolverError::RetrievalError(Arc::new(
                                        QueryPlannerError::Timeout(e.to_string()),
                                    )))?
                                }
                            }
                        } else {
                            match task.await {
                                    Err(e) if exceeded_memory_limit.load(Ordering::Relaxed) => {
                                        record_outcome_if_none(
                                            &outcome_recorded_for_memory_limit,
                                            Outcome::Cancelled,
                                        );
                                        Err(CacheResolverError::RetrievalError(Arc::new(
                                        QueryPlannerError::MemoryLimitExceeded(e.to_string()),
                                        )))?
                                    },
                                    result => result,
                            }
                        }
                    }
                    Mode::Measure => {
                        // In measure mode, spawn a timeout task that only records outcome
                        let _maybe_timeout_guard =  self.cooperative_cancellation.timeout().map(|timeout| {
                           let timeout_task = tokio::task::spawn(async move {
                               tokio::time::sleep(timeout).await;
                               record_outcome_if_none(
                                   &outcome_recorded_for_timeout,
                                   Outcome::Timeout,
                               );
                           });

                            scopeguard::guard(timeout_task.abort_handle(), |abort_handle| {
                                   abort_handle.abort();
                            })
                        });

                        // In measure mode, spawn a memory limit task that only records outcome
                        if let Some(memory_limit) = self.cooperative_cancellation.memory_limit() {
                            if let Some(stats) = crate::allocator::current() {
                                let notify_memory_limit_exceeded = Arc::new(tokio::sync::Notify::new());
                                let notify_memory_limit_exceeded_listener = notify_memory_limit_exceeded.clone();
                                let memory_limit_task = tokio::task::spawn(async move {
                                    notify_memory_limit_exceeded_listener.notified().await;
                                    record_outcome_if_none(
                                        &outcome_recorded_for_memory_limit,
                                        Outcome::Cancelled,
                                    );
                                });

                                let _memory_limit_guard = scopeguard::guard(memory_limit_task.abort_handle(), |abort_handle| {
                                    abort_handle.abort();
                                });

                                let memory_limit_bytes = memory_limit.as_u64() as usize;
                                stats.set_allocation_limit(memory_limit_bytes, Box::new(move |_bytes_allocated| {
                                    notify_memory_limit_exceeded.notify_waiters();
                                    log::warn!("memory limit exceeded planning query: {}", &query);
                                }));
                                tokio::task::spawn(planning_task).await
                            } else {
                                log::error!("memory limit cooperative cancellation is set but no stats are available");
                                tokio::task::spawn(planning_task).await
                            }
                        } else {
                            tokio::task::spawn(planning_task).await
                        }
                    }
                }
            } else {
                // some clients might timeout and cancel the request before query planning is finished,
                // so we execute it in a task that can continue even after the request was canceled and
                // the join handle was dropped. That way, the next similar query will use the cache instead
                // of restarting the query planner until another timeout
                tokio::task::spawn(planning_task).await
            }
            .inspect(|res| {
                // We won't reach this code path if the plan was cancelled, and
                // thus it won't overwrite the outcome.
                match res {
                    Ok(_) => {
                        record_outcome_if_none(&outcome_recorded, Outcome::Success);
                    }
                    Err(CacheResolverError::RetrievalError(e)) => {
                        if matches!(e.as_ref(), QueryPlannerError::Timeout(_)) {
                            record_outcome_if_none(&outcome_recorded, Outcome::Timeout);
                        } else {
                            record_outcome_if_none(&outcome_recorded, Outcome::Error);
                        };
                    }
                    Err(CacheResolverError::Backpressure(_)) => {
                        record_outcome_if_none(&outcome_recorded, Outcome::Backpressure);
                    }
                    Err(CacheResolverError::BatchingError(_)) => {
                        record_outcome_if_none(&outcome_recorded, Outcome::BatchingError);
                    }
                };
            })
            .map_err(convert_join_error)?
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

                        crate::spec::operation_limits::check_measured(
                            &plan.query_metrics,
                            &self.config_limits,
                            &caching_key.query,
                            caching_key.operation.as_deref(),
                        )
                        .map_err(|e| CacheResolverError::RetrievalError(Arc::new(e.into())))?;
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
    use std::collections::HashMap;
    use std::time::Duration;

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    use bytesize::ByteSize;
    use mockall::mock;
    use parking_lot::Mutex;
    use serde_json_bytes::json;
    use test_log::test;
    use tower::Service;
    use tracing::Subscriber;
    use tracing_core::Field;
    use tracing_subscriber::Layer;
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::Context as TracingContext;
    use tracing_subscriber::prelude::*;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::apollo_studio_interop::UsageReporting;
    use crate::configuration::QueryPlanning;
    use crate::configuration::Supergraph;
    use crate::json_ext::Object;
    use crate::query_planner::QueryPlan;
    use crate::spec::Query;
    use crate::spec::Schema;

    // Custom layer that records any field updates on spans.
    #[derive(Default, Clone)]
    struct RecordingLayer {
        values: Arc<Mutex<HashMap<String, String>>>,
    }

    impl RecordingLayer {
        fn get(&self, key: &str) -> Option<String> {
            self.values.lock().get(key).cloned()
        }
    }

    impl<S> Layer<S> for RecordingLayer
    where
        S: Subscriber,
    {
        fn on_record(
            &self,
            _span: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _ctx: TracingContext<'_, S>,
        ) {
            let mut guard = self.values.lock();
            struct Visitor<'a> {
                map: &'a mut HashMap<String, String>,
            }

            impl<'a> tracing_core::field::Visit for Visitor<'a> {
                fn record_str(&mut self, field: &Field, value: &str) {
                    self.map.insert(field.name().to_string(), value.to_string());
                }

                fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                    self.map
                        .insert(field.name().to_string(), format!("{value:?}"));
                }
            }

            let mut visitor = Visitor { map: &mut guard };
            values.record(&mut visitor);
        }
    }

    // Helper function to set up tracing for tests
    fn setup_tracing() -> (RecordingLayer, tracing::subscriber::DefaultGuard) {
        let layer = RecordingLayer::default();
        let subscriber = Registry::default().with(layer.clone());
        let guard = tracing::subscriber::set_default(subscriber);
        (layer, guard)
    }

    // Unified SlowQueryPlanner that can work in both enforce and measure modes
    #[derive(Clone)]
    struct SlowQueryPlanner {
        enforce: bool,
    }

    impl Service<QueryPlannerRequest> for SlowQueryPlanner {
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
            let enforce = self.enforce;
            Box::pin(async move {
                // Sleep for a long time - this should trigger timeout
                tokio::time::sleep(Duration::from_secs(2)).await;
                if enforce {
                    panic!("This query planner should not be called, as it is expected to timeout");
                } else {
                    // In measurement mode, this should complete successfully even after timeout
                    let plan = Arc::new(QueryPlan::fake_new(None, None));
                    Ok(QueryPlannerResponse::builder()
                        .content(QueryPlannerContent::Plan { plan })
                        .build())
                }
            })
        }
    }

    // Unified ExcessiveMemoryQueryPlanner that can work in both enforce and measure modes
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[derive(Clone)]
    struct ExcessiveMemoryQueryPlanner {
        enforce: bool,
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    impl Service<QueryPlannerRequest> for ExcessiveMemoryQueryPlanner {
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
            let enforce = self.enforce;
            Box::pin(async move {
                // Allocate 10MB of memory
                let _ = Vec::<u8>::with_capacity(10_000_000);
                if enforce {
                    futures::future::pending().await
                } else {
                    // In measurement mode, this should complete successfully even after exceeding the memory limit
                    let plan = Arc::new(QueryPlan::fake_new(None, None));
                    Ok(QueryPlannerResponse::builder()
                        .content(QueryPlannerContent::Plan { plan })
                        .build())
                }
            })
        }
    }

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

        let configuration = Configuration::default();

        let doc1 = Query::parse_document(
            "query Me { me { username } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc1));

        for _ in 0..5 {
            assert!(
                planner
                    .call(query_planner::CachingRequest::new(
                        "query Me { me { username } }".to_string(),
                        Some("".into()),
                        context.clone(),
                        Default::default()
                    ))
                    .await
                    .is_err()
            );
        }
        let doc2 = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc2));

        assert!(
            planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { name { first } } }".to_string(),
                    Some("".into()),
                    context.clone(),
                    Default::default()
                ))
                .await
                .is_err()
        );
    }

    #[test(tokio::test)]
    async fn test_cooperative_cancellation_timeout() {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::enabled_with_timeout(
                                    std::time::Duration::from_secs(1),
                                ),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            SlowQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .await;

        match result {
            Ok(_) => panic!("Expected an error, but got a response"),
            Err(e) => {
                assert!(matches!(e, CacheResolverError::RetrievalError(_)));
                assert!(e.to_string().contains("timed out"));
            }
        }

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the timeout outcome
        assert_eq!(layer.get("outcome"), Some("timeout".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_cooperative_cancellation_memory_limit() {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::enforce_with_memory_limit(ByteSize::mb(
                                    10,
                                )),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            ExcessiveMemoryQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .with_memory_tracking("planning_task")
            .await;

        match result {
            Ok(_) => panic!("Expected an error, but got a response"),
            Err(e) => {
                assert!(matches!(e, CacheResolverError::RetrievalError(_)));
                assert!(e.to_string().contains("memory limit exceeded"));
            }
        }

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the cancelled outcome
        assert_eq!(layer.get("outcome"), Some("cancelled".to_string()));
    }

    #[test(tokio::test)]
    async fn test_cooperative_cancellation_client_drop() {
        use std::sync::Arc;

        use tokio::sync::Barrier;

        let (layer, _guard) = setup_tracing();
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();

        #[derive(Clone)]
        struct SlowQueryPlanner {
            barrier: Arc<Barrier>,
        }

        impl Service<QueryPlannerRequest> for SlowQueryPlanner {
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
                let barrier = self.barrier.clone();
                Box::pin(async move {
                    // Signal that we've started
                    barrier.wait().await;

                    // Now sleep for a long time - this should get cancelled
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    panic!(
                        "This query planner should not complete, as it should be cancelled by client drop"
                    );
                })
            }
        }

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::enabled(),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            SlowQueryPlanner {
                barrier: barrier_clone,
            },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);

        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        // Spawn the planning task
        let planning_task = tokio::spawn(async move {
            planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { name { first } } }".to_string(),
                    Some("".into()),
                    context.clone(),
                    Default::default(),
                ))
                .await
        });

        // Wait for the inner SlowQueryPlanner task to start
        barrier.wait().await;

        // Now abort the outer task - the inner task should have definitely started
        planning_task.abort();

        // Verify the task was cancelled
        match planning_task.await {
            Ok(_) => panic!(
                "Expected the task to be aborted due to client drop, but it completed successfully"
            ),
            Err(e) => assert!(e.is_cancelled(), "Task should be cancelled, got: {e:?}"),
        }

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the cancelled outcome
        assert_eq!(layer.get("outcome"), Some("cancelled".to_string()));
    }

    #[test(tokio::test)]
    async fn test_cooperative_cancellation_measurement_mode_timeout() {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::measure_with_timeout(
                                    std::time::Duration::from_millis(100),
                                ),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            SlowQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        // In measurement mode, the request should complete successfully even though it times out
        // The timeout should be recorded as an outcome, but the request should not fail
        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .await;

        // In measurement mode, the request should succeed even though it times out
        assert!(
            result.is_ok(),
            "Expected success in measurement mode, got error"
        );

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the timeout outcome (not success)
        // In measurement mode, we should record timeout and not overwrite it with success
        assert_eq!(layer.get("outcome"), Some("timeout".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_cooperative_cancellation_measurement_mode_memory_limit() {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::measure_with_memory_limit(ByteSize::mb(
                                    10,
                                )),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            ExcessiveMemoryQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        // In measurement mode, the request should complete successfully even though it times out
        // The timeout should be recorded as an outcome, but the request should not fail
        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .with_memory_tracking("planning_task")
            .await;

        // In measurement mode, the request should succeed even though it times out
        assert!(
            result.is_ok(),
            "Expected success in measurement mode, got error"
        );

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the timeout outcome (not success)
        // In measurement mode, we should record timeout and not overwrite it with success
        assert_eq!(layer.get("outcome"), Some("cancelled".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_cooperative_cancellation_both_timeout_and_memory_limit_timeout_first() {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::enforce_with_timeout_and_memory_limit(
                                    std::time::Duration::from_millis(100),
                                    ByteSize::mb(100),
                                ),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            SlowQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .with_memory_tracking("planning_task")
            .await;

        match result {
            Ok(_) => panic!("Expected an error, but got a response"),
            Err(e) => {
                assert!(matches!(e, CacheResolverError::RetrievalError(_)));
                assert!(e.to_string().contains("timed out"));
            }
        }

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the timeout outcome (timeout should trigger first)
        assert_eq!(layer.get("outcome"), Some("timeout".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_cooperative_cancellation_both_timeout_and_memory_limit_memory_first() {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::enforce_with_timeout_and_memory_limit(
                                    std::time::Duration::from_secs(10),
                                    ByteSize::mb(10),
                                ),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            ExcessiveMemoryQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .with_memory_tracking("planning_task")
            .await;

        match result {
            Ok(_) => panic!("Expected an error, but got a response"),
            Err(e) => {
                assert!(matches!(e, CacheResolverError::RetrievalError(_)));
                assert!(e.to_string().contains("memory limit exceeded"));
            }
        }

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the cancelled outcome (memory limit should trigger first)
        assert_eq!(layer.get("outcome"), Some("cancelled".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_cooperative_cancellation_measure_mode_both_timeout_and_memory_limit_timeout_first()
     {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::measure_with_timeout_and_memory_limit(
                                    std::time::Duration::from_millis(100),
                                    ByteSize::mb(100),
                                ),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            SlowQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        // In measurement mode, the request should complete successfully even though it times out
        // The timeout should be recorded as an outcome, but the request should not fail
        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .with_memory_tracking("planning_task")
            .await;

        // In measurement mode, the request should succeed even though it times out
        assert!(
            result.is_ok(),
            "Expected success in measurement mode, got error"
        );

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the timeout outcome (not success)
        // In measurement mode, we should record timeout and not overwrite it with success
        assert_eq!(layer.get("outcome"), Some("timeout".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_cooperative_cancellation_measure_mode_both_timeout_and_memory_limit_memory_first()
    {
        let (layer, _guard) = setup_tracing();

        let configuration = Configuration::builder()
            .and_supergraph(Some(
                Supergraph::builder()
                    .query_planning(
                        QueryPlanning::builder()
                            .experimental_cooperative_cancellation(
                                CooperativeCancellation::measure_with_timeout_and_memory_limit(
                                    std::time::Duration::from_secs(10),
                                    ByteSize::mb(10),
                                ),
                            )
                            .build(),
                    )
                    .build(),
            ))
            .build()
            .expect("configuration is valid");
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::new(
            ExcessiveMemoryQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
            IndexMap::default(),
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            "query Me { me { name { first } } }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        // Create a span with the outcome field declared
        let span = tracing::info_span!("test_span", outcome = tracing::field::Empty);
        // Keep the span alive and ensure it's the current span during the entire operation
        let _span_guard = span.enter();

        // In measurement mode, the request should complete successfully even though it exceeds memory limit
        // The memory limit should be recorded as an outcome, but the request should not fail
        let result = planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone(),
                Default::default(),
            ))
            .with_memory_tracking("planning_task")
            .await;

        // In measurement mode, the request should succeed even though it exceeds memory limit
        assert!(
            result.is_ok(),
            "Expected success in measurement mode, got error"
        );

        // Give a small delay to ensure the span is recorded
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Verify that the span recorded the cancelled outcome (not success)
        // In measurement mode, we should record cancelled and not overwrite it with success
        assert_eq!(layer.get("outcome"), Some("cancelled".to_string()));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test(tokio::test)]
    async fn test_allocation_stats_recorded_without_cooperative_cancellation() {
        use crate::allocator::AllocationStats;

        async {
            let configuration = Configuration::default();
            let schema_str = include_str!("testdata/schema.graphql");
            let schema = Arc::new(Schema::parse(schema_str, &configuration).unwrap());

            let planning_task_stats: Arc<Mutex<Option<Arc<AllocationStats>>>> =
                Arc::new(Mutex::new(None));

            let planning_task_stats_delegate = planning_task_stats.clone();

            let mut delegate = MockMyQueryPlanner::new();
            delegate.expect_clone().returning(move || {
                let mut planner = MockMyQueryPlanner::new();
                let planning_task_stats_clone = planning_task_stats_delegate.clone();

                planner.expect_sync_call().times(0..2).returning(move |_| {
                    let plan = Arc::new(QueryPlan::fake_new(None, None));
                    *planning_task_stats_clone.lock() = crate::allocator::current();

                    Ok(QueryPlannerResponse::builder()
                        .content(QueryPlannerContent::Plan { plan })
                        .build())
                });
                planner
            });

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
                "query Me { me { name { first } } }",
                None,
                &schema,
                &configuration,
            )
            .unwrap();

            let context = Context::new();
            context
                .extensions()
                .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

            // Call the CachingQueryPlanner (no cooperative cancellation configured)
            let _ = planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { name { first } } }".to_string(),
                    Some("".into()),
                    context.clone(),
                    Default::default(),
                ))
                .await;

            let stats = planning_task_stats
                .lock()
                .clone()
                .expect("planning task stats should be set");
            assert_eq!(stats.name(), "planning_task");
        }
        .with_memory_tracking("test_request")
        .await;
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

        let context = crate::Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        for _ in 0..5 {
            let _ = planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { username } }".to_string(),
                    Some("".into()),
                    context.clone(),
                    Default::default(),
                ))
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

        let configuration = Default::default();
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

        let configuration = Configuration::default();

        let doc1 = Query::parse_document(
            "{
              __schema {
                  types {
                  name
                }
              }
            }",
            None,
            &schema,
            &configuration,
        )
        .unwrap();

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc1));

        assert!(
            planner
                .call(query_planner::CachingRequest::new(
                    "{
                    __schema {
                        types {
                        name
                      }
                    }
                  }"
                    .to_string(),
                    Some("".into()),
                    context.clone(),
                    Default::default()
                ))
                .await
                .is_ok()
        );

        assert!(
            planner
                .call(query_planner::CachingRequest::new(
                    "{
                        __schema {
                            types {
                            name
                          }
                        }
                      }"
                    .to_string(),
                    Some("".into()),
                    context.clone(),
                    Default::default()
                ))
                .await
                .is_ok()
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
                Default::default(),
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
                Default::default(),
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
                Default::default(),
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
                Default::default(),
            ))
            .await;

        if let (Err(e), Err(e2)) = (r, r2) {
            assert_eq!(e.to_string(), e2.to_string());
        } else {
            panic!("Expected both calls to return same error");
        }
    }

    #[tokio::test]
    async fn test_cache_warmup() {
        let create_delegate = |call_count| {
            let mut delegate = MockMyQueryPlanner::new();
            delegate.expect_clone().times(1).returning(move || {
                let mut planner = MockMyQueryPlanner::new();
                planner.expect_sync_call().times(call_count).returning(|_| {
                    let plan = Arc::new(QueryPlan::fake_new(None, None));
                    Ok(QueryPlannerResponse::builder()
                        .content(QueryPlannerContent::Plan { plan })
                        .build())
                });
                planner
            });
            delegate
        };

        let configuration: Configuration = Default::default();
        let schema = Arc::new(
            Schema::parse(
                include_str!("../testdata/starstuff@current.graphql"),
                &configuration,
            )
            .unwrap(),
        );

        let create_planner = async |delegate| {
            CachingQueryPlanner::new(
                delegate,
                schema.clone(),
                Default::default(),
                &configuration,
                IndexMap::default(),
            )
            .await
            .unwrap()
        };

        let create_request = || {
            let query_str = "query ExampleQuery { me { name } }".to_string();
            let doc = Query::parse_document(&query_str, None, &schema, &configuration).unwrap();
            let context = Context::new();
            context
                .extensions()
                .with_lock(|lock| lock.insert::<ParsedDocument>(doc));
            query_planner::CachingRequest::new(query_str, None, context, Default::default())
        };

        // send query to caching planner. it should save this query plan in its cache
        let mut planner = create_planner(create_delegate(1)).await;
        let response = planner.call(create_request()).await.unwrap();
        assert!(response.content.is_some());
        assert_eq!(planner.cache.len().await, 1);

        // create and warm up a new planner. new planner's delegate should be called once during
        // the warm-up phase to populate the cache
        let query_analysis_layer =
            QueryAnalysisLayer::new(schema.clone(), Arc::new(configuration.clone())).await;
        let mut new_planner = create_planner(create_delegate(1)).await;
        new_planner
            .warm_up(
                &query_analysis_layer,
                &Arc::new(PersistedQueryLayer::new(&configuration).await.unwrap()),
                Some(planner.previous_cache()),
                Some(1),
                Default::default(),
                &Default::default(),
            )
            .await;
        // wait a beat - items are added to cache asynchronously, so this helps avoid flakiness
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(new_planner.cache.len().await, 1);

        // create a new delegate that _shouldn't_ be called since the new planner already has the
        // result in its cache
        new_planner.delegate = create_delegate(0);
        let response = new_planner.call(create_request()).await.unwrap();
        assert!(response.content.is_some());
        assert_eq!(new_planner.cache.len().await, 1);
    }
}
