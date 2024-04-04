use std::num::NonZeroUsize;
use std::sync::Arc;

use async_channel::bounded;
use async_channel::Sender;
use futures::future::BoxFuture;
use router_bridge::planner::Planner;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tower::ServiceExt;

use super::bridge_query_planner::BridgeQueryPlanner;
use super::QueryPlanResult;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Schema;
use crate::Configuration;

static CHANNEL_SIZE: usize = 10_000;

#[derive(Clone)]
pub(crate) struct BridgeQueryPlannerPool {
    planners: Vec<BridgeQueryPlanner>,
    sender: Sender<(
        QueryPlannerRequest,
        oneshot::Sender<Result<QueryPlannerResponse, QueryPlannerError>>,
    )>,
    schema: Arc<Schema>,
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

        let mut planners = Vec::new();

        while let Some(task_result) = join_set.join_next().await {
            let planner =
                task_result.map_err(|e| ServiceBuildError::ServiceError(Box::new(e)))??;

            let receiver = receiver.clone();
            let inner = planner.clone();
            tokio::spawn(async move {
                while let Ok((request, res_sender)) = receiver.recv().await {
                    let res = inner.clone().oneshot(request).await;

                    if res_sender.send(res).is_err() {
                        failfast_error!("receiver channel for query plan response was closed, this should never happen");
                    }
                }
            });

            planners.push(planner);
        }

        let schema = planners
            .first()
            .expect("There should be at least 1 service in pool")
            .schema();

        Ok(Self {
            planners,
            sender,
            schema,
        })
    }

    pub(crate) fn planners(&self) -> Vec<Arc<Planner<QueryPlanResult>>> {
        self.planners.iter().map(|b| b.planner().clone()).collect()
    }

    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
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
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
        let (response_sender, response_receiver) = oneshot::channel();

        let sender = self.sender.clone();
        Box::pin(async move {
            let _ = sender.send((req, response_sender)).await;

            response_receiver
                .await
                .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?
        })
    }
}
