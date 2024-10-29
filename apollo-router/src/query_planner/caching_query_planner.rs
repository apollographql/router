use std::collections::HashMap;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Deref;
use std::sync::Arc;
use std::task;

use apollo_compiler::validation::Valid;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use query_planner::QueryPlannerPlugin;
use rand::seq::SliceRandom;
use rand::thread_rng;
use router_bridge::planner::PlanOptions;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use router_bridge::planner::UsageReporting;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use super::dual_query_planner::opt_plan_node_matches;
use super::fetch::QueryHash;
use crate::cache::estimate_size;
use crate::cache::storage::InMemoryCache;
use crate::cache::storage::ValueType;
use crate::cache::DeduplicatingCache;
use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
use crate::configuration::QueryPlanReuseMode;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::telemetry::utils::Timer;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::query_planner::labeler::add_defer_labels;
use crate::query_planner::BridgeQueryPlannerPool;
use crate::query_planner::QueryPlanResult;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::query_planner;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;
use crate::Context;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn QueryPlannerPlugin>>;
pub(crate) type InMemoryCachePlanner =
    InMemoryCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>;
pub(crate) const APOLLO_OPERATION_ID: &str = "apollo_operation_id";

#[derive(Debug, Clone, Hash)]
pub(crate) enum ConfigMode {
    //FIXME: add the Rust planner structure once it is hashable and serializable,
    // for now use the JS config as it expected to be identical to the Rust one
    Rust(Arc<apollo_federation::query_plan::query_planner::QueryPlannerConfig>),
    BothBestEffort(Arc<QueryPlannerConfig>),
    Js(Arc<QueryPlannerConfig>),
}

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner<T: Clone> {
    cache: Arc<
        DeduplicatingCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>,
    >,
    delegate: T,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    plugins: Arc<Plugins>,
    enable_authorization_directives: bool,
    experimental_reuse_query_plans: QueryPlanReuseMode,
    config_mode: Arc<QueryHash>,
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
            Error = QueryPlannerError,
        > + Send,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) async fn new(
        delegate: T,
        schema: Arc<Schema>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
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
        match configuration.experimental_query_planner_mode {
            crate::configuration::QueryPlannerMode::New => {
                "PLANNER-NEW".hash(&mut hasher);
                ConfigMode::Rust(Arc::new(configuration.rust_query_planner_config()))
                    .hash(&mut hasher);
            }
            crate::configuration::QueryPlannerMode::Legacy => {
                "PLANNER-LEGACY".hash(&mut hasher);
                ConfigMode::Js(Arc::new(configuration.js_query_planner_config())).hash(&mut hasher);
            }
            crate::configuration::QueryPlannerMode::Both => {
                "PLANNER-BOTH".hash(&mut hasher);
                ConfigMode::Js(Arc::new(configuration.js_query_planner_config())).hash(&mut hasher);
                ConfigMode::Rust(Arc::new(configuration.rust_query_planner_config()))
                    .hash(&mut hasher);
            }
            crate::configuration::QueryPlannerMode::BothBestEffort => {
                ConfigMode::BothBestEffort(Arc::new(configuration.js_query_planner_config()))
                    .hash(&mut hasher);
            }
        };
        let config_mode = Arc::new(QueryHash(hasher.finalize()));

        Ok(Self {
            cache,
            delegate,
            schema,
            subgraph_schemas,
            plugins: Arc::new(plugins),
            enable_authorization_directives,
            experimental_reuse_query_plans: configuration
                .supergraph
                .query_planning
                .experimental_reuse_query_plans,
            config_mode,
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
        experimental_reuse_query_plans: QueryPlanReuseMode,
        experimental_pql_prewarm: &PersistedQueriesPrewarmQueryPlanCache,
    ) {
        let _timer = Timer::new(|duration| {
            ::tracing::info!(
                histogram.apollo.router.query.planning.warmup.duration = duration.as_secs_f64()
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
                                config_mode: _,
                            },
                            _,
                        )| WarmUpCachingQueryKey {
                            query: query.clone(),
                            operation_name: operation.clone(),
                            hash: Some(hash.clone()),
                            metadata: metadata.clone(),
                            plan_options: plan_options.clone(),
                            config_mode: self.config_mode.clone(),
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
                        config_mode: self.config_mode.clone(),
                    });
                }
            }
        }

        all_cache_keys.extend(cache_keys.into_iter());

        let mut count = 0usize;
        let mut reused = 0usize;
        let mut could_have_reused = 0usize;
        for WarmUpCachingQueryKey {
            mut query,
            operation_name,
            hash,
            metadata,
            plan_options,
            config_mode: _,
        } in all_cache_keys
        {
            let context = Context::new();
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
                hash: if experimental_reuse_query_plans == QueryPlanReuseMode::Reuse {
                    CachingQueryHash::Reuse(doc.hash.clone())
                } else {
                    CachingQueryHash::DoNotReuse {
                        query_hash: doc.hash.clone(),
                        schema_hash: self.schema.schema_id.clone(),
                    }
                },
                metadata: metadata.clone(),
                plan_options: plan_options.clone(),
                config_mode: self.config_mode.clone(),
            };

            let mut should_measure = None;
            if let Some(warmup_hash) = hash.clone() {
                if experimental_reuse_query_plans == QueryPlanReuseMode::Reuse {
                    if let Some(ref previous_cache) = &previous_cache {
                        // if the query hash did not change with the schema update, we can reuse the previously cached entry
                        if warmup_hash.schema_aware_query_hash() == &*doc.hash {
                            if let Some(entry) =
                                { previous_cache.lock().await.get(&caching_key).cloned() }
                            {
                                self.cache.insert_in_memory(caching_key, entry).await;
                                reused += 1;
                                continue;
                            }
                        }
                    }
                } else if self.experimental_reuse_query_plans == QueryPlanReuseMode::Measure
                    && warmup_hash.schema_aware_query_hash() == &*doc.hash
                {
                    should_measure = Some(CachingQueryKey {
                        query: query.clone(),
                        operation: operation_name.clone(),
                        hash: warmup_hash.clone(),
                        metadata: metadata.clone(),
                        plan_options: plan_options.clone(),
                        config_mode: self.config_mode.clone(),
                    });
                }
            };

            let entry = self
                .cache
                .get(&caching_key, |v| {
                    init_query_plan_from_redis(&self.subgraph_schemas, v)
                })
                .await;
            if entry.is_first() {
                let doc = match query_analysis
                    .parse_document(&query, operation_name.as_deref())
                    .await
                {
                    Ok(doc) => doc,
                    Err(error) => {
                        let e = Arc::new(QueryPlannerError::SpecError(error));
                        tokio::spawn(async move {
                            entry.insert(Err(e)).await;
                        });
                        continue;
                    }
                };

                let schema = self.schema.api_schema();
                if let Ok(modified_query) = add_defer_labels(schema, &doc.ast) {
                    query = modified_query.to_string();
                }

                context.extensions().with_lock(|mut lock| {
                    lock.insert::<ParsedDocument>(doc);
                    lock.insert(caching_key.metadata)
                });

                let request = QueryPlannerRequest {
                    query: query.clone(),
                    operation_name: operation_name.clone(),
                    context: context.clone(),
                };

                let res = match service.ready().await {
                    Ok(service) => service.call(request).await,
                    Err(_) => break,
                };

                match res {
                    Ok(QueryPlannerResponse { content, .. }) => {
                        if let Some(content) = content.clone() {
                            count += 1;

                            // we want to measure query plan reuse
                            if let Some(reused_cache_key) = should_measure {
                                if let Some(previous) = &previous_cache {
                                    let previous_plan = {
                                        let mut cache = previous.lock().await;
                                        cache.get(&reused_cache_key).cloned()
                                    };

                                    if let Some(previous_content) =
                                        previous_plan.and_then(|res| res.ok())
                                    {
                                        if let (
                                            QueryPlannerContent::Plan {
                                                plan: previous_plan,
                                            },
                                            QueryPlannerContent::Plan { plan: new_plan },
                                        ) = (previous_content, &content)
                                        {
                                            let matched = opt_plan_node_matches(
                                                &Some(&*previous_plan.root),
                                                &Some(&*new_plan.root),
                                            );

                                            if matched.is_ok() {
                                                could_have_reused += 1;
                                            }
                                            u64_counter!(
                                                "apollo.router.operations.query_planner.reuse",
                                                "Measure possible mismatches when reusing query plans",
                                                1,
                                                "is_matched" = matched.is_ok()
                                            );
                                        }
                                    }
                                }
                            }

                            tokio::spawn(async move {
                                entry.insert(Ok(content.clone())).await;
                            });
                        }
                    }
                    Err(error) => {
                        count += 1;
                        let e = Arc::new(error);
                        tokio::spawn(async move {
                            entry.insert(Err(e)).await;
                        });
                    }
                }
            }
        }

        tracing::debug!("warmed up the query planner cache with {count} queries planned and {reused} queries reused");

        match experimental_reuse_query_plans {
            QueryPlanReuseMode::DoNotReuse => {}
            QueryPlanReuseMode::Reuse => {
                u64_counter!(
                    "apollo.router.query.planning.warmup.reused",
                    "The number of query plans that were reused instead of regenerated during query planner warm up",
                    reused as u64,
                    query_plan_reuse_active = true
                );
            }
            QueryPlanReuseMode::Measure => {
                u64_counter!(
                    "apollo.router.query.planning.warmup.reused",
                    "The number of query plans that were reused instead of regenerated during query planner warm up",
                    could_have_reused as u64,
                    query_plan_reuse_active = false
                );
            }
        }
    }
}

impl CachingQueryPlanner<BridgeQueryPlannerPool> {
    pub(crate) fn js_planners(&self) -> Vec<Arc<Planner<QueryPlanResult>>> {
        self.delegate.js_planners()
    }

    pub(crate) fn subgraph_schemas(
        &self,
    ) -> Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>> {
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
        Error = QueryPlannerError,
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
            qp.plan(request).await.inspect(|response| {
                if let Some(usage_reporting) = context
                    .extensions()
                    .with_lock(|lock| lock.get::<Arc<UsageReporting>>().cloned())
                {
                    let _ = response.context.insert(
                        APOLLO_OPERATION_ID,
                        stats_report_key_hash(usage_reporting.stats_report_key.as_str()),
                    );
                    let _ = response.context.insert(
                        "apollo_operation_signature",
                        usage_reporting.stats_report_key.clone(),
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
            Error = QueryPlannerError,
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
            hash: if self.experimental_reuse_query_plans == QueryPlanReuseMode::Reuse {
                CachingQueryHash::Reuse(doc.hash.clone())
            } else {
                CachingQueryHash::DoNotReuse {
                    query_hash: doc.hash.clone(),
                    schema_hash: self.schema.schema_id.clone(),
                }
            },
            metadata,
            plan_options,
            config_mode: self.config_mode.clone(),
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
                mut query,
                operation_name,
                context,
            } = request;

            let schema = self.schema.api_schema();
            if let Ok(modified_query) = add_defer_labels(schema, &doc.ast) {
                query = modified_query.to_string();
            }

            let request = QueryPlannerRequest::builder()
                .query(query)
                .and_operation_name(operation_name)
                .context(context)
                .build();

            // some clients might timeout and cancel the request before query planning is finished,
            // so we execute it in a task that can continue even after the request was canceled and
            // the join handle was dropped. That way, the next similar query will use the cache instead
            // of restarting the query planner until another timeout
            tokio::task::spawn(
                async move {
                    let res = self.delegate.ready().await?.call(request).await;

                    match res {
                        Ok(QueryPlannerResponse {
                            content,
                            context,
                            errors,
                        }) => {
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
                                }
                            }

                            // This will be overridden by the Rust usage reporting implementation
                            if let Some(QueryPlannerContent::Plan { plan, .. }) = &content {
                                context.extensions().with_lock(|mut lock| {
                                    lock.insert::<Arc<UsageReporting>>(plan.usage_reporting.clone())
                                });
                            }
                            Ok(QueryPlannerResponse {
                                content,
                                context,
                                errors,
                            })
                        }
                        Err(error) => {
                            let e = Arc::new(error);
                            let err = e.clone();
                            tokio::spawn(async move {
                                entry.insert(Err(err)).await;
                            });
                            Err(CacheResolverError::RetrievalError(e))
                        }
                    }
                }
                .in_current_span(),
            )
            .await
            .map_err(|e| {
                CacheResolverError::RetrievalError(Arc::new(QueryPlannerError::JoinError(
                    e.to_string(),
                )))
            })?
        } else {
            let res = entry
                .get()
                .await
                .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?;

            match res {
                Ok(content) => {
                    if let QueryPlannerContent::Plan { plan, .. } = &content {
                        context.extensions().with_lock(|mut lock| {
                            lock.insert::<Arc<UsageReporting>>(plan.usage_reporting.clone())
                        });
                    }

                    Ok(QueryPlannerResponse::builder()
                        .content(content)
                        .context(context)
                        .build())
                }
                Err(error) => {
                    match error.deref() {
                        QueryPlannerError::PlanningErrors(pe) => {
                            request.context.extensions().with_lock(|mut lock| {
                                lock.insert::<Arc<UsageReporting>>(Arc::new(
                                    pe.usage_reporting.clone(),
                                ))
                            });
                        }
                        QueryPlannerError::SpecError(e) => {
                            request.context.extensions().with_lock(|mut lock| {
                                lock.insert::<Arc<UsageReporting>>(Arc::new(UsageReporting {
                                    stats_report_key: e.get_error_key().to_string(),
                                    referenced_fields_by_type: HashMap::new(),
                                }))
                            });
                        }
                        _ => {}
                    }

                    Err(CacheResolverError::RetrievalError(error))
                }
            }
        }
    }
}

fn stats_report_key_hash(stats_report_key: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(stats_report_key.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CachingQueryKey {
    pub(crate) query: String,
    pub(crate) operation: Option<String>,
    pub(crate) hash: CachingQueryHash,
    pub(crate) metadata: CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
    pub(crate) config_mode: Arc<QueryHash>,
}

// Update this key every time the cache key or the query plan format has to change.
// When changed it MUST BE CALLED OUT PROMINENTLY IN THE CHANGELOG.
const CACHE_KEY_VERSION: usize = 1;
const FEDERATION_VERSION: &str = std::env!("FEDERATION_VERSION");

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
        self.config_mode.hash(&mut hasher);
        let metadata = hex::encode(hasher.finalize());

        write!(
            f,
            "plan:cache:{}:federation:{}:{}:opname:{}:metadata:{}",
            CACHE_KEY_VERSION, FEDERATION_VERSION, self.hash, operation, metadata,
        )
    }
}

// TODO: this is an intermediate type to hold the query hash while query plan reuse is still experimental
// this will be replaced by the schema aware query hash once the option is removed
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CachingQueryHash {
    Reuse(Arc<QueryHash>),
    DoNotReuse {
        query_hash: Arc<QueryHash>,
        schema_hash: Arc<String>,
    },
}

impl CachingQueryHash {
    fn schema_aware_query_hash(&self) -> &QueryHash {
        match self {
            CachingQueryHash::Reuse(hash) => hash,
            CachingQueryHash::DoNotReuse { query_hash, .. } => query_hash,
        }
    }
}

impl Hash for CachingQueryHash {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            CachingQueryHash::Reuse(hash) => hash.hash(state),
            CachingQueryHash::DoNotReuse {
                schema_hash,
                query_hash,
            } => {
                schema_hash.hash(state);
                query_hash.hash(state);
            }
        }
    }
}

impl std::fmt::Display for CachingQueryHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CachingQueryHash::Reuse(hash) => write!(f, "query:{}", hash),
            CachingQueryHash::DoNotReuse {
                schema_hash,
                query_hash,
            } => write!(f, "schema:{}:query:{}", schema_hash, query_hash),
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct WarmUpCachingQueryKey {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) hash: Option<CachingQueryHash>,
    pub(crate) metadata: CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
    pub(crate) config_mode: Arc<QueryHash>,
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
    use mockall::mock;
    use mockall::predicate::*;
    use router_bridge::planner::UsageReporting;
    use test_log::test;
    use tower::Service;

    use super::*;
    use crate::error::PlanErrors;
    use crate::json_ext::Object;
    use crate::query_planner::QueryPlan;
    use crate::spec::Query;
    use crate::spec::Schema;
    use crate::Configuration;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_call(
                &self,
                key: QueryPlannerRequest,
            ) -> Result<QueryPlannerResponse, QueryPlannerError>;
        }

        impl Clone for MyQueryPlanner {
            fn clone(&self) -> MockMyQueryPlanner;
        }
    }

    impl Service<QueryPlannerRequest> for MockMyQueryPlanner {
        type Response = QueryPlannerResponse;

        type Error = QueryPlannerError;

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
            planner.expect_sync_call().times(0..2).returning(|_| {
                Err(QueryPlannerError::from(PlanErrors {
                    errors: Default::default(),
                    usage_reporting: UsageReporting {
                        stats_report_key: "this is a test key".to_string(),
                        referenced_fields_by_type: Default::default(),
                    },
                }))
            });
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
            .with_lock(|mut lock| lock.insert::<ParsedDocument>(doc1));

        for _ in 0..5 {
            assert!(planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { username } }".to_string(),
                    Some("".into()),
                    context.clone()
                ))
                .await
                .is_err());
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
            .with_lock(|mut lock| lock.insert::<ParsedDocument>(doc2));

        assert!(planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone()
            ))
            .await
            .is_err());
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
                    usage_reporting: UsageReporting {
                        stats_report_key: "this is a test report key".to_string(),
                        referenced_fields_by_type: Default::default(),
                    }
                    .into(),
                    query: Arc::new(Query::empty()),
                    query_metrics: Default::default(),
                    estimated_size: Default::default(),
                };
                let qp_content = QueryPlannerContent::Plan {
                    plan: Arc::new(query_plan),
                };

                Ok(QueryPlannerResponse::builder()
                    .content(qp_content)
                    .context(Context::new())
                    .build())
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
            .with_lock(|mut lock| lock.insert::<ParsedDocument>(doc));

        for _ in 0..5 {
            assert!(planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { username } }".to_string(),
                    Some("".into()),
                    context.clone(),
                ))
                .await
                .unwrap()
                .context
                .extensions()
                .with_lock(|lock| lock.contains_key::<Arc<UsageReporting>>()));
        }
    }

    #[test]
    fn apollo_operation_id_hash() {
        assert_eq!(
            "d1554552698157b05c2a462827fb4367a4548ee5",
            stats_report_key_hash("# IgnitionMeQuery\nquery IgnitionMeQuery{me{id}}")
        );
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

                    Ok(QueryPlannerResponse::builder()
                        .content(qp_content)
                        .context(Context::new())
                        .build())
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
            .with_lock(|mut lock| lock.insert::<ParsedDocument>(doc1));

        assert!(planner
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
            ))
            .await
            .is_ok());

        assert!(planner
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
            ))
            .await
            .is_ok());
    }
}
