use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::task;

use futures::future::BoxFuture;
use sha2::Digest;
use sha2::Sha256;
use tokio_util::time::FutureExt;
#[cfg(test)]
use tower::BoxError;
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
use crate::configuration::cooperative_cancellation::CooperativeCancellation;
use crate::configuration::mode::Mode;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::query_planner::SubgraphSchemas;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::services::query_parsing::ParsedDocument;
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
        }
    }
}

pub(crate) type QueryPlanCache = Arc<
    DeduplicatingCache<
        CachingQueryKey,
        Result<QueryPlannerContent, Arc<QueryPlannerError>>,
        ComputeBackPressureError,
    >,
>;

pub(crate) type InMemoryQueryPlanCache =
    InMemoryCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>;

pub(crate) const APOLLO_OPERATION_ID: &str = "apollo::supergraph::operation_id";

/// Hashed value of query planner configuration for use in cache keys.
#[derive(Clone, Hash, PartialEq, Eq)]
// XXX(@goto-bus-stop): I think this probably should not be pub(crate), but right now all fields in
// the cache keys are pub(crate), which I'm not going to change at this time :)
pub(crate) struct ConfigModeHash(Vec<u8>);

impl ConfigModeHash {
    pub(crate) fn from_configuration(configuration: &Configuration) -> Self {
        let mut hasher = StructHasher::new();
        configuration.rust_query_planner_config().hash(&mut hasher);
        Self(hasher.finalize())
    }
}

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
///
/// ## Context
/// Required context keys:
/// - [`ParsedDocument`]
///
/// Optional context keys:
/// - [`ComputeJobType`] (defaults to [`ComputeJobType::QueryPlanning`])
/// - "apollo::authentication::jwt_claims"
/// - "apollo::authorization::required_scopes"
/// - "apollo::authorization::required_policies"
/// - "apollo::progressive_override::labels_to_override"
///
/// Inserts context:
/// - `Arc<`[`UsageReporting`]`>`
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner<T> {
    cache: QueryPlanCache,
    delegate: T,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    enable_authorization_directives: bool,
    config_mode_hash: Arc<ConfigModeHash>,
    cooperative_cancellation: CooperativeCancellation,
}

fn init_query_plan_from_redis(
    subgraph_schemas: &SubgraphSchemas,
    cache_entry: &mut Result<QueryPlannerContent, Arc<QueryPlannerError>>,
) -> Result<(), String> {
    if let Ok(plan) = cache_entry {
        // Arc freshly deserialized from Redis should be unique, so this doesn't clone:
        let plan = Arc::make_mut(plan);
        if let Some(root) = plan.root.as_mut() {
            let root = Arc::make_mut(root);
            root.init_parsed_operations(subgraph_schemas)
                .map_err(|e| format!("Invalid subgraph operation: {e}"))?
        }
    }
    Ok(())
}

impl<T> CachingQueryPlanner<T> {
    #[cfg(test)]
    pub(crate) async fn for_test(
        delegate: T,
        schema: Arc<Schema>,
        subgraph_schemas: Arc<SubgraphSchemas>,
        configuration: &Configuration,
    ) -> Result<Self, BoxError> {
        let cache = crate::pipeline::build_query_plan_cache(
            configuration,
            crate::pipeline::connect_query_plan_redis(configuration).await?,
        );
        Ok(Self::new(
            delegate,
            schema,
            subgraph_schemas,
            configuration,
            cache,
        ))
    }

    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) fn new(
        delegate: T,
        schema: Arc<Schema>,
        subgraph_schemas: Arc<SubgraphSchemas>,
        configuration: &Configuration,
        cache: QueryPlanCache,
    ) -> Self {
        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(configuration, &schema);

        let config_mode_hash = Arc::new(ConfigModeHash::from_configuration(configuration));
        let cooperative_cancellation = configuration
            .supergraph
            .query_planning
            .experimental_cooperative_cancellation
            .clone();

        Self {
            cache,
            delegate,
            schema,
            subgraph_schemas,
            enable_authorization_directives,
            cooperative_cancellation,
            config_mode_hash,
        }
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
        // We don't propagate backpressure from the query planner itself,
        // because compared to short-lived work, query planning can take a long time and is
        // more sensitive to pool saturation. In that case, we want the router to stop serving
        // requests containing _new_ queries, but it's still capable of serving requests with
        // already planned queries.
        // XXX(@goto-bus-stop): to maintain this behaviour once we adopt apollo-cache layers, we can
        // add a load shed layer on the query planner.
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
    /// Plan a query, first hitting the cache.
    ///
    /// Uses context keys:
    /// - apollo::authentication::jwt_claims
    /// - apollo::authorization::required_scopes
    /// - apollo::authorization::required_policies
    /// - apollo::progressive_override::labels_to_override
    /// - [`ParsedDocument`]
    /// - [`ComputeJobType`]
    ///
    /// Inserts context:
    /// - `Arc<`[`UsageReporting`]`>`
    async fn plan(
        mut self,
        request: query_planner::CachingRequest,
    ) -> Result<<T as Service<QueryPlannerRequest>>::Response, CacheResolverError> {
        let query_planner::CachingRequest {
            query,
            operation_name,
            context,
        } = request;

        if self.enable_authorization_directives {
            AuthorizationPlugin::update_cache_key(&context);
        }

        let plan_options = PlanOptions {
            override_conditions: context
                .get(LABELS_TO_OVERRIDE_KEY)
                .unwrap_or_default()
                .unwrap_or_default(),
        };

        let Some(doc) = context
            .extensions()
            .with_lock(|lock| lock.get::<ParsedDocument>().cloned())
        else {
            return Err(CacheResolverError::RetrievalError(Arc::new(
                // FIXME(@goto-bus-stop): we should make it impossible to call the query planning
                // service without having a ParsedDocument available.
                QueryPlannerError::SpecError(SpecError::TransformError(
                    "missing parsed document".to_string(),
                )),
            )));
        };

        let metadata = context
            .extensions()
            .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned())
            .unwrap_or_default();

        let compute_job_type = context
            .extensions()
            .with_lock(|lock| lock.get::<ComputeJobType>().copied())
            .unwrap_or(ComputeJobType::QueryPlanning);

        // Build the inner query planner request.
        let request = QueryPlannerRequest::builder()
            .query(&query)
            .and_operation_name(operation_name.clone())
            .document(doc.clone())
            .metadata(metadata.clone())
            .plan_options(plan_options.clone())
            .compute_job_type(compute_job_type)
            .build();

        // Check the cache first
        let caching_key = CachingQueryKey {
            query: query.clone(),
            operation: operation_name.clone(),
            hash: doc.hash.clone(),
            metadata,
            plan_options,
            schema_id: self.schema.schema_id.clone(),
            config_mode_hash: self.config_mode_hash.clone(),
        };

        let entry = self
            .cache
            .get(&caching_key, |v| {
                init_query_plan_from_redis(&self.subgraph_schemas, v)
            })
            .await;

        if entry.is_first() {
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
                            tokio::spawn(async move {
                                entry.insert(Ok(content)).await;
                            });
                        }

                        // This will be overridden by the Rust usage reporting implementation
                        if let Some(plan) = &content {
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
                    let plan = &content;
                    context.extensions().with_lock(|lock| {
                        lock.insert::<Arc<UsageReporting>>(plan.usage_reporting.clone())
                    });

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
            Ok(plan) => Some(plan.estimated_size()),
            Err(e) => Some(estimate_size(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    use bytesize::ByteSize;
    use parking_lot::Mutex;
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
    use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
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
                    Ok(QueryPlannerResponse::builder().content(plan).build())
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
                    Ok(QueryPlannerResponse::builder().content(plan).build())
                }
            })
        }
    }

    /// Adapts a `tower_test::mock::Mock`'s boxed error back into the concrete
    /// `MaybeBackPressureError<QueryPlannerError>` that `CachingQueryPlanner` requires.
    fn downcast_mock_error(err: BoxError) -> MaybeBackPressureError<QueryPlannerError> {
        *err.downcast::<MaybeBackPressureError<QueryPlannerError>>()
            .expect("mock should only ever send MaybeBackPressureError<QueryPlannerError>")
    }

    #[test(tokio::test)]
    async fn test_plan() {
        let (mock, mut handle) =
            tower_test::mock::pair::<QueryPlannerRequest, QueryPlannerResponse>();
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        let driver = tokio::task::spawn(async move {
            while let Some((_request, responder)) = handle.next_request().await {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                responder.send_error(MaybeBackPressureError::PermanentError(
                    QueryPlannerError::UnhandledPlannerResult,
                ));
            }
        });

        let configuration = Arc::new(crate::Configuration::default());
        let schema = include_str!("testdata/schema.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::for_test(
            mock.map_err(downcast_mock_error),
            schema.clone(),
            Default::default(),
            &configuration,
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

        let query1 = "query Me { me { username } }".to_string();
        assert!(
            planner
                .call(query_planner::CachingRequest::new(
                    query1.clone(),
                    Some("".into()),
                    context.clone()
                ))
                .await
                .is_err()
        );
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // Cache insertion is spawned asynchronously; wait until a follow-up call is
        // served from cache (delegate call count stops increasing).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut last_count = call_count.load(Ordering::SeqCst);
        loop {
            assert!(
                planner
                    .call(query_planner::CachingRequest::new(
                        query1.clone(),
                        Some("".into()),
                        context.clone()
                    ))
                    .await
                    .is_err()
            );
            let count = call_count.load(Ordering::SeqCst);
            if count == last_count {
                break;
            }
            last_count = count;
            assert!(
                tokio::time::Instant::now() < deadline,
                "permanent error was not cached (delegate kept being invoked)"
            );
            tokio::task::yield_now().await;
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

        let cached_count = call_count.load(Ordering::SeqCst);
        assert!(
            planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { name { first } } }".to_string(),
                    Some("".into()),
                    context.clone()
                ))
                .await
                .is_err()
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            cached_count + 1,
            "a different query must miss the cache and invoke the delegate"
        );

        drop(planner);
        crate::plugin::test::await_mock_driver(driver).await;
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

        let mut planner = CachingQueryPlanner::for_test(
            SlowQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            ExcessiveMemoryQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            SlowQueryPlanner {
                barrier: barrier_clone,
            },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            SlowQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            ExcessiveMemoryQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            SlowQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            ExcessiveMemoryQueryPlanner { enforce: true },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            SlowQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
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

        let mut planner = CachingQueryPlanner::for_test(
            ExcessiveMemoryQueryPlanner { enforce: false },
            schema.clone(),
            Default::default(),
            &configuration,
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

    macro_rules! test_query_plan {
        () => {
            include_str!("testdata/query_plan.json")
        };
    }

    #[test(tokio::test)]
    async fn test_usage_reporting() {
        let (mock, mut handle) =
            tower_test::mock::pair::<QueryPlannerRequest, QueryPlannerResponse>();
        let driver = tokio::task::spawn(async move {
            let query_plan: QueryPlan = QueryPlan {
                formatted_query_plan: Default::default(),
                root: serde_json::from_str(test_query_plan!()).unwrap(),
                usage_reporting: UsageReporting::Error("this is a test report key".to_string())
                    .into(),
                query: Arc::new(Query::empty_for_tests()),
                estimated_size: Default::default(),
            };
            let plan = Arc::new(query_plan);

            while let Some((_request, responder)) = handle.next_request().await {
                let qp_content = plan.clone();
                responder
                    .send_response(QueryPlannerResponse::builder().content(qp_content).build());
            }
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

        let mut planner = CachingQueryPlanner::for_test(
            mock.map_err(downcast_mock_error),
            Arc::new(schema),
            Default::default(),
            &configuration,
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
                ))
                .await
                .unwrap();
            assert!(
                context
                    .extensions()
                    .with_lock(|lock| lock.contains_key::<Arc<UsageReporting>>())
            );
        }

        drop(planner);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    // Expect that if we call the CQP twice, the second call will return cached data
    #[test(tokio::test)]
    async fn test_cache_works() {
        let (mock, mut handle) =
            tower_test::mock::pair::<QueryPlannerRequest, QueryPlannerResponse>();
        let driver = tokio::task::spawn(async move {
            let (_request, responder) = handle
                .next_request()
                .await
                .expect("should receive one request");

            let content = Arc::new(QueryPlan::fake_new(None, None));

            responder.send_response(QueryPlannerResponse::builder().content(content).build());
        });

        let configuration = Default::default();
        let schema = include_str!("../testdata/starstuff@current.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut service = CachingQueryPlanner::for_test(
            mock.map_err(|err| panic!("tower-test errored: {err}")),
            schema.clone(),
            Default::default(),
            &configuration,
        )
        .await
        .unwrap();

        let query = "query ExampleQuery { me { name } }";
        let doc = Query::parse_document(query, None, &schema, &configuration).unwrap();
        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        let _ = service
            .ready()
            .await
            .unwrap()
            .call(query_planner::CachingRequest::new(
                query.to_string(),
                None,
                context.clone(),
            ))
            .await
            .unwrap();

        let _ = service
            .ready()
            .await
            .unwrap()
            .call(query_planner::CachingRequest::new(
                query.to_string(),
                None,
                context.clone(),
            ))
            .await
            .unwrap();

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    /// Drives the planner mock, counting requests and answering each with `content`.
    fn spawn_counting_planner(
        mut handle: tower_test::mock::Handle<QueryPlannerRequest, QueryPlannerResponse>,
        content: QueryPlannerContent,
    ) -> (tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let driver = tokio::task::spawn(async move {
            while let Some((_request, responder)) = handle.next_request().await {
                calls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                responder.send_response(
                    QueryPlannerResponse::builder()
                        .content(content.clone())
                        .build(),
                );
            }
        });
        (driver, calls)
    }

    /// A configuration and schema pair that enables auth directives to work.
    fn authorization_enabled_config_and_schema() -> (Configuration, Arc<Schema>) {
        let configuration: Configuration = serde_json::from_value(serde_json::json!({
            "authorization": { "directives": { "enabled": true } }
        }))
        .unwrap();
        let schema = include_str!("../../tests/fixtures/supergraph-auth.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());
        (configuration, schema)
    }

    /// Builds a request the way the router does: authorization state travels in the
    /// context as JWT claims, and `plan` derives `CacheKeyMetadata` from them via
    /// `AuthorizationPlugin::update_cache_key`, overwriting any metadata inserted into
    /// the context directly.
    fn authorization_caching_request(
        query: &str,
        schema: &Schema,
        configuration: &Configuration,
        authenticated: bool,
    ) -> query_planner::CachingRequest {
        let doc = Query::parse_document(query, None, schema, configuration).unwrap();
        let context = Context::new();
        if authenticated {
            context
                .insert(APOLLO_AUTHENTICATION_JWT_CLAIMS, "placeholder".to_string())
                .unwrap();
        }
        context.extensions().with_lock(|lock| {
            lock.insert::<ParsedDocument>(doc);
        });
        query_planner::CachingRequest::new(query.to_string(), None, context)
    }

    /// `CacheKeyMetadata` is part of `CachingQueryKey`'s `Hash`/`Eq`, so the same query
    /// under different authorization state reaches the inner planner again. That keeps an
    /// unauthenticated request from receiving a plan built for an authenticated one.
    #[test(tokio::test)]
    async fn plan_cache_is_segmented_by_authorization_metadata() {
        let (mock, handle) = tower_test::mock::pair::<QueryPlannerRequest, QueryPlannerResponse>();
        let (driver, planner_calls) =
            spawn_counting_planner(handle, Arc::new(QueryPlan::fake_new(None, None)));

        let (configuration, schema) = authorization_enabled_config_and_schema();
        let mut service = CachingQueryPlanner::for_test(
            mock.map_err(|err| panic!("tower-test errored: {err}")),
            schema.clone(),
            Default::default(),
            &configuration,
        )
        .await
        .unwrap();

        let query = "query ExampleQuery { me { name } }";

        for authenticated in [
            false, true, // Repeats the first key, which must now hit the cache.
            false,
        ] {
            service
                .ready()
                .await
                .unwrap()
                .call(authorization_caching_request(
                    query,
                    &schema,
                    &configuration,
                    authenticated,
                ))
                .await
                .unwrap();
        }

        assert_eq!(
            planner_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "each distinct authorization state must be planned separately, \
             and a repeated state must be served from cache"
        );

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    /// The cache stores an emptied operation's plan like any other — and, like any
    /// other, it stays keyed by authorization state, so a plan cached for an
    /// unauthenticated request is never served to an authenticated one.
    #[test(tokio::test)]
    async fn emptied_operation_plan_is_cached() {
        let (mock, handle) = tower_test::mock::pair::<QueryPlannerRequest, QueryPlannerResponse>();
        // The plan `QueryPlannerService::get` returns for an emptied operation: no
        // root node.
        let emptied_query = Query::empty_for_tests();
        let emptied_plan = QueryPlan {
            usage_reporting: Arc::new(UsageReporting::Operation(Default::default())),
            root: None,
            formatted_query_plan: None,
            query: Arc::new(emptied_query),
            estimated_size: Default::default(),
        };
        let (driver, planner_calls) = spawn_counting_planner(handle, Arc::new(emptied_plan));

        let (configuration, schema) = authorization_enabled_config_and_schema();
        let mut service = CachingQueryPlanner::for_test(
            mock.map_err(|err| panic!("tower-test errored: {err}")),
            schema.clone(),
            Default::default(),
            &configuration,
        )
        .await
        .unwrap();

        let query = "query ExampleQuery { me { name } }";

        for _ in 0..2 {
            service
                .ready()
                .await
                .unwrap()
                .call(authorization_caching_request(
                    query,
                    &schema,
                    &configuration,
                    false,
                ))
                .await
                .unwrap();
        }

        assert_eq!(
            planner_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second identical request must be served from cache"
        );

        service
            .ready()
            .await
            .unwrap()
            .call(authorization_caching_request(
                query,
                &schema,
                &configuration,
                true,
            ))
            .await
            .unwrap();

        assert_eq!(
            planner_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a cached rejection must not be served to a request in a different \
             authorization state"
        );

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[test(tokio::test)]
    async fn test_temporary_errors_arent_cached() {
        let (mock, mut handle) =
            tower_test::mock::pair::<QueryPlannerRequest, QueryPlannerResponse>();
        let driver = tokio::task::spawn(async move {
            while let Some((_request, responder)) = handle.next_request().await {
                responder.send_error(MaybeBackPressureError::<QueryPlannerError>::TemporaryError(
                    ComputeBackPressureError,
                ));
            }
        });

        let configuration = Default::default();
        let schema = include_str!("../testdata/starstuff@current.graphql");
        let schema = Arc::new(Schema::parse(schema, &configuration).unwrap());

        let mut planner = CachingQueryPlanner::for_test(
            mock.map_err(downcast_mock_error),
            schema.clone(),
            Default::default(),
            &configuration,
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
            .ready()
            .await
            .unwrap()
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
            .ready()
            .await
            .unwrap()
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

        drop(planner);
        crate::plugin::test::await_mock_driver(driver).await;
    }
}
