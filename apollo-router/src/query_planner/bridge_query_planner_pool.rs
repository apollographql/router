use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::validation::Valid;
use async_channel::bounded;
use async_channel::Sender;
use futures::future::BoxFuture;
use opentelemetry::metrics::MeterProvider;
use router_bridge::planner::Planner;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tower::Service;
use tower::ServiceExt;

use super::bridge_query_planner::BridgeQueryPlanner;
use super::QueryPlanResult;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::metrics::meter_provider;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Schema;
use crate::Configuration;

static CHANNEL_SIZE: usize = 1_000;

#[derive(Clone)]
pub(crate) struct BridgeQueryPlannerPool {
    planners: Vec<Arc<Planner<QueryPlanResult>>>,
    sender: Sender<(
        QueryPlannerRequest,
        oneshot::Sender<Result<QueryPlannerResponse, QueryPlannerError>>,
    )>,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    _pool_size_gauge: opentelemetry::metrics::ObservableGauge<u64>,
}

impl BridgeQueryPlannerPool {
    pub(crate) async fn new(
        sdl: String,
        configuration: Arc<Configuration>,
        size: NonZeroUsize,
    ) -> Result<Self, ServiceBuildError> {
        Self::new_from_planners(Default::default(), sdl, configuration, size).await
    }

    pub(crate) async fn new_from_planners(
        old_planners: Vec<Arc<Planner<QueryPlanResult>>>,
        schema: String,
        configuration: Arc<Configuration>,
        size: NonZeroUsize,
    ) -> Result<Self, ServiceBuildError> {
        let mut join_set = JoinSet::new();

        let (sender, receiver) = bounded::<(
            QueryPlannerRequest,
            oneshot::Sender<Result<QueryPlannerResponse, QueryPlannerError>>,
        )>(CHANNEL_SIZE);

        let mut old_planners_iterator = old_planners.into_iter();

        (0..size.into()).for_each(|_| {
            let sdl = schema.clone();
            let configuration = configuration.clone();

            if let Some(old_planner) = old_planners_iterator.next() {
                join_set.spawn(async move {
                    BridgeQueryPlanner::new_from_planner(old_planner, sdl, configuration).await
                });
            } else {
                join_set.spawn(async move { BridgeQueryPlanner::new(sdl, configuration).await });
            }
        });

        let mut bridge_query_planners = Vec::new();

        while let Some(task_result) = join_set.join_next().await {
            let bridge_query_planner =
                task_result.map_err(|e| ServiceBuildError::ServiceError(Box::new(e)))??;
            bridge_query_planners.push(bridge_query_planner);
        }

        let schema = bridge_query_planners
            .first()
            .ok_or_else(|| {
                ServiceBuildError::QueryPlannerError(QueryPlannerError::PoolProcessing(
                    "There should be at least 1 Query Planner service in pool".to_string(),
                ))
            })?
            .schema();

        let subgraph_schemas = bridge_query_planners
            .first()
            .ok_or_else(|| {
                ServiceBuildError::QueryPlannerError(QueryPlannerError::PoolProcessing(
                    "There should be at least 1 Query Planner service in pool".to_string(),
                ))
            })?
            .subgraph_schemas();

        let planners = bridge_query_planners
            .iter()
            .map(|p| p.planner().clone())
            .collect();

        for (worker_id, mut planner) in bridge_query_planners.into_iter().enumerate() {
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
                    let start = Instant::now();

                    let res = svc.call(request).await;

                    f64_histogram!(
                        "apollo.router.query_planning.plan.duration",
                        "Duration of the query planning.",
                        start.elapsed().as_secs_f64(),
                        "workerId" = worker_id.to_string()
                    );

                    let _ = res_sender.send(res);
                }
            });
        }
        let sender_for_gauge = sender.clone();
        let pool_size_gauge = meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.query_planning.queued")
            .with_callback(move |m| m.observe(sender_for_gauge.len() as u64, &[]))
            .init();

        Ok(Self {
            planners,
            sender,
            schema,
            subgraph_schemas,
            _pool_size_gauge: pool_size_gauge,
        })
    }

    pub(crate) fn planners(&self) -> Vec<Arc<Planner<QueryPlanResult>>> {
        self.planners.clone()
    }

    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    pub(crate) fn subgraph_schemas(
        &self,
    ) -> Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>> {
        self.subgraph_schemas.clone()
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

        Box::pin(async move {
            let start = Instant::now();
            let _ = sender.send((req, response_sender)).await;

            let res = response_receiver
                .await
                .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?;

            f64_histogram!(
                "apollo.router.query_planning.total.duration",
                "Duration of the time the router waited for a query plan, including both the queue time and planning time.",
                start.elapsed().as_secs_f64(),
                []
            );

            res
        })
    }
}
