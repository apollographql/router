use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Poll;
use std::time::Instant;

use apollo_compiler::validation::Valid;
use futures::future::BoxFuture;
use opentelemetry::metrics::ObservableGauge;

use super::bridge_query_planner::BridgeQueryPlanner;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::introspection::IntrospectionCache;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Schema;
use crate::Configuration;

#[derive(Clone)]
pub(crate) struct BridgeQueryPlannerPool {
    pool_mode: PoolMode,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    compute_jobs_queue_size_gauge: Arc<Mutex<Option<ObservableGauge<u64>>>>,
    introspection_cache: Arc<IntrospectionCache>,
}

// TODO: remove
#[derive(Clone)]
enum PoolMode {
    PassThrough { delegate: BridgeQueryPlanner },
}

impl BridgeQueryPlannerPool {
    pub(crate) async fn new(
        schema: Arc<Schema>,
        configuration: Arc<Configuration>,
    ) -> Result<Self, ServiceBuildError> {
        // All query planners in the pool now share the same introspection cache.
        // This allows meaningful gauges, and it makes sense that queries should be cached across all planners.
        let introspection_cache = Arc::new(IntrospectionCache::new(&configuration));

        let delegate =
            BridgeQueryPlanner::new(schema.clone(), configuration, introspection_cache.clone())
                .await?;

        Ok(Self {
            subgraph_schemas: delegate.subgraph_schemas(),
            pool_mode: PoolMode::PassThrough { delegate },
            schema,
            compute_jobs_queue_size_gauge: Default::default(),
            introspection_cache,
        })
    }

    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    pub(crate) fn subgraph_schemas(
        &self,
    ) -> Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>> {
        self.subgraph_schemas.clone()
    }

    pub(super) fn activate(&self) {
        // Gauges MUST be initialized after a meter provider is created.
        // When a hot reload happens this means that the gauges must be re-initialized.
        *self
            .compute_jobs_queue_size_gauge
            .lock()
            .expect("lock poisoned") = Some(crate::compute_job::create_queue_size_gauge());
        self.introspection_cache.activate();
    }
}

impl tower::Service<QueryPlannerRequest> for BridgeQueryPlannerPool {
    type Response = QueryPlannerResponse;

    type Error = QueryPlannerError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        if crate::compute_job::is_full() {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
        let pool_mode = self.pool_mode.clone();
        Box::pin(async move {
            let start;
            let res = match pool_mode {
                PoolMode::PassThrough { mut delegate } => {
                    start = Instant::now();
                    delegate.call(req).await
                }
            };

            f64_histogram!(
                "apollo.router.query_planning.total.duration",
                "Duration of the time the router waited for a query plan, including both the queue time and planning time.",
                start.elapsed().as_secs_f64()
            );

            res
        })
    }
}
