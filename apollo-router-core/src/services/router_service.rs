use crate::services::execution_service::ExecutionService;
use crate::{
    PlannedRequest, Plugin, RouterBridgeQueryPlanner, RouterRequest, RouterResponse, Schema,
    SubgraphRequest,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct RouterService<QueryPlannerService, ExecutionService> {
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

pub struct PluggableRouterServiceBuilder {
    schema: Arc<Schema>,
    concurrency: usize,
    plugins: Vec<Box<dyn Plugin>>,
    services: Vec<(
        String,
        BoxService<SubgraphRequest, RouterResponse, BoxError>,
    )>,
}

impl PluggableRouterServiceBuilder {
    pub fn new(schema: Arc<Schema>, concurrency: usize) -> Self {
        Self {
            schema,
            concurrency,
            plugins: Default::default(),
            services: Default::default(),
        }
    }

    pub fn with_plugin<E: Plugin + 'static>(mut self, plugin: E) -> PluggableRouterServiceBuilder {
        self.plugins.push(Box::new(plugin));
        self
    }

    pub fn with_subgraph_service<
        S: Service<
                SubgraphRequest,
                Response = RouterResponse,
                Error = Box<(dyn std::error::Error + Send + Sync + 'static)>,
            > + Send
            + 'static,
    >(
        mut self,
        name: &str,
        service: S,
    ) -> PluggableRouterServiceBuilder
    where
        <S as Service<SubgraphRequest>>::Future: Send,
    {
        self.services.push((name.to_string(), service.boxed()));
        self
    }

    pub fn build(mut self) -> BoxCloneService<RouterRequest, RouterResponse, BoxError> {
        //Reverse the order of the plugins for usability
        self.plugins.reverse();

        //QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let query_planner_service = ServiceBuilder::new()
            .boxed_clone()
            .buffer(self.concurrency)
            .service(self.plugins.iter_mut().fold(
                RouterBridgeQueryPlanner::new(self.schema.clone()).boxed(),
                |acc, e| e.query_planning_service(acc),
            ));

        //SubgraphService takes a SubgraphRequest and outputs a RouterResponse
        let subgraphs = self
            .services
            .into_iter()
            .map(|(name, s)| {
                (
                    name.clone(),
                    ServiceBuilder::new().service(
                        self.plugins
                            .iter_mut()
                            .fold(s, |acc, e| e.subgraph_service(&name, acc)),
                    ),
                )
            })
            .collect();

        //ExecutionService takes a PlannedRequest and outputs a RouterResponse
        let execution_service = ServiceBuilder::new()
            .boxed_clone()
            .buffer(self.concurrency)
            .service(
                self.plugins.iter_mut().fold(
                    ExecutionService::builder()
                        .schema(self.schema.clone())
                        .subgraph_services(self.concurrency, subgraphs)
                        .build()
                        .boxed(),
                    |acc, e| e.execution_service(acc),
                ),
            );

        //Router service takes a graphql::Request and outputs a graphql::Response
        let router_service = ServiceBuilder::new()
            .boxed_clone()
            .buffer(self.concurrency)
            .service(
                self.plugins.iter_mut().fold(
                    RouterService::builder()
                        .query_planner_service(query_planner_service)
                        .query_execution_service(execution_service)
                        .build()
                        .boxed(),
                    |acc, e| e.router_service(acc),
                ),
            );

        router_service
    }
}
