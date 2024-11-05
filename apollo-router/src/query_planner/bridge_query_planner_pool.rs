use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use apollo_compiler::validation::Valid;
use async_channel::bounded;
use async_channel::Sender;
use futures::future::BoxFuture;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::Unit;
use router_bridge::planner::Planner;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tower::Service;
use tower::ServiceExt;

use super::bridge_query_planner::BridgeQueryPlanner;
use super::QueryPlanResult;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::introspection::IntrospectionCache;
use crate::metrics::meter_provider;
use crate::query_planner::PlannerMode;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Schema;
use crate::Configuration;

static CHANNEL_SIZE: usize = 1_000;

#[derive(Clone)]
pub(crate) struct BridgeQueryPlannerPool {
    js_planners: Vec<Arc<Planner<QueryPlanResult>>>,
    sender: Sender<(
        QueryPlannerRequest,
        oneshot::Sender<Result<QueryPlannerResponse, QueryPlannerError>>,
    )>,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pool_size_gauge: Arc<Mutex<Option<ObservableGauge<u64>>>>,
    v8_heap_used: Arc<AtomicU64>,
    v8_heap_used_gauge: Arc<Mutex<Option<ObservableGauge<u64>>>>,
    v8_heap_total: Arc<AtomicU64>,
    v8_heap_total_gauge: Arc<Mutex<Option<ObservableGauge<u64>>>>,
    introspection_cache: Arc<IntrospectionCache>,
}

impl BridgeQueryPlannerPool {
    pub(crate) async fn new(
        old_js_planners: Vec<Arc<Planner<QueryPlanResult>>>,
        schema: Arc<Schema>,
        configuration: Arc<Configuration>,
        size: NonZeroUsize,
    ) -> Result<Self, ServiceBuildError> {
        let rust_planner = PlannerMode::maybe_rust(&schema, &configuration)?;

        let mut join_set = JoinSet::new();

        let (sender, receiver) = bounded::<(
            QueryPlannerRequest,
            oneshot::Sender<Result<QueryPlannerResponse, QueryPlannerError>>,
        )>(CHANNEL_SIZE);

        let mut old_js_planners_iterator = old_js_planners.into_iter();

        // All query planners in the pool now share the same introspection cache.
        // This allows meaningful gauges, and it makes sense that queries should be cached across all planners.
        let introspection_cache = Arc::new(IntrospectionCache::new(&configuration));

        for _ in 0..size.into() {
            let schema = schema.clone();
            let configuration = configuration.clone();
            let rust_planner = rust_planner.clone();
            let introspection_cache = introspection_cache.clone();

            let old_planner = old_js_planners_iterator.next();
            join_set.spawn(async move {
                BridgeQueryPlanner::new(
                    schema,
                    configuration,
                    old_planner,
                    rust_planner,
                    introspection_cache,
                )
                .await
            });
        }

        let mut bridge_query_planners = Vec::new();

        while let Some(task_result) = join_set.join_next().await {
            let bridge_query_planner =
                task_result.map_err(|e| ServiceBuildError::ServiceError(Box::new(e)))??;
            bridge_query_planners.push(bridge_query_planner);
        }

        let subgraph_schemas = bridge_query_planners
            .first()
            .ok_or_else(|| {
                ServiceBuildError::QueryPlannerError(QueryPlannerError::PoolProcessing(
                    "There should be at least 1 Query Planner service in pool".to_string(),
                ))
            })?
            .subgraph_schemas();

        let js_planners: Vec<_> = bridge_query_planners
            .iter()
            .filter_map(|p| p.js_planner())
            .collect();

        for mut planner in bridge_query_planners.into_iter() {
            let receiver = receiver.clone();

            tokio::spawn(async move {
                while let Ok((request, res_sender)) = receiver.recv().await {
                    let svc = match planner.ready().await {
                        Ok(svc) => svc,
                        Err(e) => {
                            let _ = res_sender.send(Err(e));

                            continue;
                        }
                    };

                    let res = svc.call(request).await;

                    let _ = res_sender.send(res);
                }
            });
        }
        let v8_heap_used: Arc<AtomicU64> = Default::default();
        let v8_heap_total: Arc<AtomicU64> = Default::default();

        // initialize v8 metrics
        if let Some(bridge_query_planner) = js_planners.first().cloned() {
            Self::get_v8_metrics(
                bridge_query_planner,
                v8_heap_used.clone(),
                v8_heap_total.clone(),
            )
            .await;
        }

        Ok(Self {
            js_planners,
            sender,
            schema,
            subgraph_schemas,
            pool_size_gauge: Default::default(),
            v8_heap_used,
            v8_heap_used_gauge: Default::default(),
            v8_heap_total,
            v8_heap_total_gauge: Default::default(),
            introspection_cache,
        })
    }

    fn create_pool_size_gauge(&self) -> ObservableGauge<u64> {
        let sender = self.sender.clone();
        let meter = meter_provider().meter("apollo/router");
        meter
            .u64_observable_gauge("apollo.router.query_planning.queued")
            .with_description("Number of queries waiting to be planned")
            .with_unit(Unit::new("query"))
            .with_callback(move |m| m.observe(sender.len() as u64, &[]))
            .init()
    }

    fn create_heap_used_gauge(&self) -> ObservableGauge<u64> {
        let meter = meter_provider().meter("apollo/router");
        let current_heap_used_for_gauge = self.v8_heap_used.clone();
        let heap_used_gauge = meter
            .u64_observable_gauge("apollo.router.v8.heap.used")
            .with_description("V8 heap used, in bytes")
            .with_unit(Unit::new("By"))
            .with_callback(move |i| {
                i.observe(current_heap_used_for_gauge.load(Ordering::SeqCst), &[])
            })
            .init();
        heap_used_gauge
    }

    fn create_heap_total_gauge(&self) -> ObservableGauge<u64> {
        let meter = meter_provider().meter("apollo/router");
        let current_heap_total_for_gauge = self.v8_heap_total.clone();
        let heap_total_gauge = meter
            .u64_observable_gauge("apollo.router.v8.heap.total")
            .with_description("V8 heap total, in bytes")
            .with_unit(Unit::new("By"))
            .with_callback(move |i| {
                i.observe(current_heap_total_for_gauge.load(Ordering::SeqCst), &[])
            })
            .init();
        heap_total_gauge
    }

    pub(crate) fn js_planners(&self) -> Vec<Arc<Planner<QueryPlanResult>>> {
        self.js_planners.clone()
    }

    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    pub(crate) fn subgraph_schemas(
        &self,
    ) -> Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>> {
        self.subgraph_schemas.clone()
    }

    async fn get_v8_metrics(
        planner: Arc<Planner<QueryPlanResult>>,
        v8_heap_used: Arc<AtomicU64>,
        v8_heap_total: Arc<AtomicU64>,
    ) {
        let metrics = planner.get_heap_statistics().await;
        if let Ok(metrics) = metrics {
            v8_heap_used.store(metrics.heap_used, Ordering::SeqCst);
            v8_heap_total.store(metrics.heap_total, Ordering::SeqCst);
        }
    }

    pub(super) fn activate(&self) {
        // Gauges MUST be initialized after a meter provider is created.
        // When a hot reload happens this means that the gauges must be re-initialized.
        *self.pool_size_gauge.lock().expect("lock poisoned") = Some(self.create_pool_size_gauge());
        *self.v8_heap_used_gauge.lock().expect("lock poisoned") =
            Some(self.create_heap_used_gauge());
        *self.v8_heap_total_gauge.lock().expect("lock poisoned") =
            Some(self.create_heap_total_gauge());
        self.introspection_cache.activate();
    }
}

impl tower::Service<QueryPlannerRequest> for BridgeQueryPlannerPool {
    type Response = QueryPlannerResponse;

    type Error = QueryPlannerError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        if self.sender.is_full() {
            std::task::Poll::Ready(Err(QueryPlannerError::PoolProcessing(
                "query plan queue is full".into(),
            )))
        } else {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
        let (response_sender, response_receiver) = oneshot::channel();
        let sender = self.sender.clone();

        let get_metrics_future =
            if let Some(bridge_query_planner) = self.js_planners.first().cloned() {
                Some(Self::get_v8_metrics(
                    bridge_query_planner,
                    self.v8_heap_used.clone(),
                    self.v8_heap_total.clone(),
                ))
            } else {
                None
            };

        Box::pin(async move {
            let start = Instant::now();
            let _ = sender.send((req, response_sender)).await;

            let res = response_receiver
                .await
                .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?;

            f64_histogram!(
                "apollo.router.query_planning.total.duration",
                "Duration of the time the router waited for a query plan, including both the queue time and planning time.",
                start.elapsed().as_secs_f64()
            );

            if let Some(f) = get_metrics_future {
                // execute in a separate task to avoid blocking the request
                tokio::task::spawn(f);
            }

            res
        })
    }
}

#[cfg(test)]

mod tests {
    use opentelemetry_sdk::metrics::data::Gauge;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::spec::Query;
    use crate::Context;

    #[tokio::test]
    async fn test_v8_metrics() {
        let sdl = include_str!("../testdata/supergraph.graphql");
        let config = Arc::default();
        let schema = Arc::new(Schema::parse(sdl, &config).unwrap());

        async move {
            let mut pool = BridgeQueryPlannerPool::new(
                Vec::new(),
                schema.clone(),
                config.clone(),
                NonZeroUsize::new(2).unwrap(),
            )
            .await
            .unwrap();
            pool.activate();
            let query = "query { me { name } }".to_string();

            let doc = Query::parse_document(&query, None, &schema, &config).unwrap();
            let context = Context::new();
            context.extensions().with_lock(|mut lock| lock.insert(doc));

            pool.call(QueryPlannerRequest::new(query, None, context))
                .await
                .unwrap();

            let metrics = crate::metrics::collect_metrics();
            let heap_used = metrics.find("apollo.router.v8.heap.used").unwrap();
            let heap_total = metrics.find("apollo.router.v8.heap.total").unwrap();

            println!(
                "got heap_used: {:?}, heap_total: {:?}",
                heap_used
                    .data
                    .as_any()
                    .downcast_ref::<Gauge<u64>>()
                    .unwrap()
                    .data_points[0]
                    .value,
                heap_total
                    .data
                    .as_any()
                    .downcast_ref::<Gauge<u64>>()
                    .unwrap()
                    .data_points[0]
                    .value
            );
        }
        .with_metrics()
        .await;
    }
}
