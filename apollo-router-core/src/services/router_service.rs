use crate::services::execution_service::ExecutionService;
use crate::{
    Context, NaiveIntrospection, PlannedRequest, Plugin, QueryCache, RouterBridgeQueryPlanner,
    RouterRequest, RouterResponse, Schema, SubgraphRequest,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;
use tracing::instrument::WithSubscriber;
use tracing::{Dispatch, Instrument};
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct RouterService<QueryPlannerService, ExecutionService> {
    query_planner_service: QueryPlannerService,
    query_execution_service: ExecutionService,
    #[builder(default)]
    ready_query_planner_service: Option<QueryPlannerService>,
    #[builder(default)]
    ready_query_execution_service: Option<ExecutionService>,
    schema: Arc<Schema>,
    query_cache: Arc<QueryCache>,
    naive_introspection: NaiveIntrospection,
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
        // We need to obtain references to two hot services for use in call.
        // The reason for us to clone here is that the async block needs to own the hot services,
        // and cloning will produce a cold service. Therefore cloning in `RouterService#call` is not
        // a valid course of action.
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
        //Consume our cloned services and allow ownership to be transferred to the async block.
        let mut planning = self.ready_query_planner_service.take().unwrap();
        let mut execution = self.ready_query_execution_service.take().unwrap();

        let schema = self.schema.clone();
        let query_cache = self.query_cache.clone();

        if let Some(response) = self
            .naive_introspection
            .get(&request.http_request.body().query)
        {
            return Box::pin(async move {
                Ok(RouterResponse {
                    response: http::Response::new(response),
                    context: Context::new().with_request(Arc::new(request.http_request)),
                })
            });
        }

        let fut = async move {
            let query = query_cache
                .get_query(&request.http_request.body().query)
                .instrument(tracing::info_span!("query_parsing"))
                .await;

            if let Some(err) = query.as_ref().and_then(|q| {
                q.validate_variables(&request.http_request.body(), &schema)
                    .err()
            }) {
                Ok(RouterResponse {
                    response: http::Response::new(err),
                    context: Context::new().with_request(Arc::new(request.http_request)),
                })
            } else {
                let operation_name = request.http_request.body().operation_name.clone();
                let planned_query = planning.call(request).await;
                let mut response = match planned_query {
                    Ok(planned_query) => execution.call(planned_query).await,
                    Err(err) => Err(err),
                };

                if let Ok(response) = &mut response {
                    if let Some(query) = query {
                        tracing::debug_span!("format_response").in_scope(move || {
                            query.format_response(
                                response.response.body_mut(),
                                operation_name.as_deref(),
                                &schema,
                            )
                        });
                    }
                }

                response
            }
        };

        Box::pin(fut)
    }
}

pub struct PluggableRouterServiceBuilder {
    schema: Arc<Schema>,
    buffer: usize,
    plugins: Vec<Box<dyn Plugin>>,
    services: Vec<(
        String,
        BoxService<SubgraphRequest, RouterResponse, BoxError>,
    )>,
    dispatcher: Dispatch,
}

impl PluggableRouterServiceBuilder {
    pub fn new(schema: Arc<Schema>, buffer: usize, dispatcher: Dispatch) -> Self {
        Self {
            schema,
            buffer,
            plugins: Default::default(),
            services: Default::default(),
            dispatcher,
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
        let (query_planner_service, query_worker) = Buffer::pair(
            ServiceBuilder::new().service(self.plugins.iter_mut().fold(
                RouterBridgeQueryPlanner::new(self.schema.clone()).boxed(),
                |acc, e| e.query_planning_service(acc),
            )),
            self.buffer,
        );
        tokio::spawn(query_worker.with_subscriber(self.dispatcher.clone()));

        //SubgraphService takes a SubgraphRequest and outputs a RouterResponse
        let subgraphs = self
            .services
            .into_iter()
            .map(|(name, s)| {
                let service = self
                    .plugins
                    .iter_mut()
                    .fold(s, |acc, e| e.subgraph_service(&name, acc));

                let (service, worker) = Buffer::pair(service, self.buffer);
                tokio::spawn(worker.with_subscriber(self.dispatcher.clone()));

                (name.clone(), service)
            })
            .collect();

        //ExecutionService takes a PlannedRequest and outputs a RouterResponse
        let (execution_service, execution_worker) = Buffer::pair(
            ServiceBuilder::new().service(
                self.plugins.iter_mut().fold(
                    ExecutionService::builder()
                        .schema(self.schema.clone())
                        .subgraph_services(subgraphs)
                        .build()
                        .boxed(),
                    |acc, e| e.execution_service(acc),
                ),
            ),
            self.buffer,
        );
        tokio::spawn(execution_worker.with_subscriber(self.dispatcher.clone()));

        let query_cache_limit = std::env::var("ROUTER_QUERY_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        let query_cache = Arc::new(QueryCache::new(query_cache_limit));

        // NaiveIntrospection instantiation can potentially block for some time
        let naive_introspection = {
            let schema = Arc::clone(&self.schema);
            // this was previously under a spawn_blocking
            NaiveIntrospection::from_schema(&schema)
        };

        /*FIXME
        // Start warming up the cache
        //
        // We don't need to do this in background because the old server will keep running until
        // this one is ready.
        //
        // If we first warm up the cache in foreground, then switch to the new config, the next
        // queries will benefit from the warmed up cache. While if we switch and warm up in
        // background, the next queries might be blocked until the cache is primed, so there'll be
        // a perf hit.
        if let Some(previous_router) = previous_router {
            for (query, operation, options) in previous_router.query_planner.get_hot_keys().await {
                // We can ignore errors because some of the queries that were previously in the
                // cache might not work with the new schema
                let _ = query_planner.get(query, operation, options).await;
            }
        }
        */
        //Router service takes a graphql::Request and outputs a graphql::Response
        let (router_service, router_worker) = Buffer::pair(
            ServiceBuilder::new().service(
                self.plugins.iter_mut().fold(
                    RouterService::builder()
                        .query_planner_service(query_planner_service)
                        .query_execution_service(execution_service)
                        .schema(self.schema)
                        .query_cache(query_cache)
                        .naive_introspection(naive_introspection)
                        .build()
                        .boxed(),
                    |acc, e| e.router_service(acc),
                ),
            ),
            self.buffer,
        );
        tokio::spawn(router_worker.with_subscriber(self.dispatcher.clone()));

        router_service.boxed_clone()
    }
}
