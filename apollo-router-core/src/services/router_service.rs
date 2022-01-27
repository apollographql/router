use crate::{PlannedRequest, RouterRequest, RouterResponse};
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use tower::BoxError;
use tower_service::Service;
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct RouterService<QueryPlannerService, ExecutionService>
where
    QueryPlannerService: Service<RouterRequest, Response = PlannedRequest, Error = BoxError>
        + Clone
        + Send
        + 'static,
    ExecutionService: Service<PlannedRequest, Response = RouterResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
{
    query_planner_service: QueryPlannerService,
    query_execution_service: ExecutionService,
    #[builder(default)]
    ready_query_planner_service: Option<QueryPlannerService>,
    #[builder(default)]
    ready_query_execution_service: Option<ExecutionService>,
}

impl<QueryPlannerService, ExecutionService> Service<RouterRequest>
    for RouterService<QueryPlannerService, ExecutionService>
where
    QueryPlannerService: Service<RouterRequest, Response = PlannedRequest, Error = BoxError>
        + Clone
        + Send
        + 'static,
    ExecutionService: Service<PlannedRequest, Response = RouterResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    QueryPlannerService::Future: Send + 'static,
    ExecutionService::Future: Send + 'static,
{
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        if vec![
            self.ready_query_planner_service
                .get_or_insert_with(|| self.query_planner_service.clone())
                .poll_ready(cx),
            self.ready_query_execution_service
                .get_or_insert_with(|| self.query_execution_service.clone())
                .poll_ready(cx),
        ]
        .iter()
        .all(|r| r.is_ready())
        {
            return Poll::Ready(Ok(()));
        }
        Poll::Pending
    }

    fn call(&mut self, request: RouterRequest) -> Self::Future {
        let mut planning = self.ready_query_planner_service.take().unwrap();
        let mut execution = self.ready_query_execution_service.take().unwrap();
        //Here we convert to an unplanned request, this is where context gets created
        let fut = async move {
            let planned_query = planning.call(request).await;
            match planned_query {
                Ok(planned_query) => execution.call(planned_query).await,
                Err(err) => Err(err),
            }
        };

        Box::pin(fut)
    }
}
