use crate::apq::APQLayer;
use crate::ensure_query_presence::EnsureQueryPresence;
use crate::forbid_http_get_mutations::ForbidHttpGetMutationsLayer;
use crate::services::execution_service::ExecutionService;
use crate::{
    plugin_utils, CachingQueryPlanner, DynPlugin, ExecutionRequest, ExecutionResponse,
    NaiveIntrospection, Plugin, QueryCache, QueryPlannerRequest, QueryPlannerResponse,
    ResponseBody, RouterBridgeQueryPlanner, RouterRequest, RouterResponse, Schema, SubgraphRequest,
    SubgraphResponse,
};
use futures::{future::BoxFuture, TryFutureExt};
use http::StatusCode;
use std::sync::Arc;
use std::task::Poll;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;
use typed_builder::TypedBuilder;

static DEFAULT_BUFFER_SIZE: usize = 20_000;

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
    QueryPlannerService: Service<QueryPlannerRequest, Response = QueryPlannerResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    ExecutionService: Service<ExecutionRequest, Response = ExecutionResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    QueryPlannerService::Future: Send + 'static,
    ExecutionService::Future: Send + 'static,
{
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

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

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        // Consume our cloned services and allow ownership to be transferred to the async block.
        let mut planning = self.ready_query_planner_service.take().unwrap();
        let mut execution = self.ready_query_execution_service.take().unwrap();

        let schema = self.schema.clone();
        let query_cache = self.query_cache.clone();

        if let Some(response) = self.naive_introspection.get(
            req.context
                .request
                .body()
                .query
                .as_ref()
                .expect("apollo.ensure-query-is-present has checked this already; qed"),
        ) {
            return Box::pin(async move {
                Ok(RouterResponse {
                    response: http::Response::new(ResponseBody::GraphQL(response)).into(),
                    context: req.context.into(),
                })
            });
        }

        let fut = async move {
            let context = req.context;
            let body = context.request.body();
            let query = query_cache
                .get_query(
                    body.query
                        .as_ref()
                        .expect("apollo.ensure-query-is-present has checked this already; qed")
                        .as_str(),
                )
                .await;

            if let Some(err) = query
                .as_ref()
                .and_then(|q| q.validate_variables(body, &schema).err())
            {
                Ok(RouterResponse {
                    response: http::Response::new(ResponseBody::GraphQL(err)).into(),
                    context: context.into(),
                })
            } else {
                let operation_name = body.operation_name.clone();
                let planned_query = planning
                    .call(QueryPlannerRequest {
                        context: context.into(),
                    })
                    .await?;
                let mut response = execution
                    .call(ExecutionRequest {
                        query_plan: planned_query.query_plan,
                        context: planned_query.context,
                    })
                    .await?;

                if let Some(query) = query {
                    tracing::debug_span!("format_response").in_scope(|| {
                        query.format_response(
                            response.response.body_mut(),
                            operation_name.as_deref(),
                            schema.api_schema(),
                        )
                    });
                }

                Ok(RouterResponse {
                    context: response.context,
                    response: response.response.map(ResponseBody::GraphQL),
                })
            }
        }
        .or_else(|error: BoxError| async move {
            Ok(plugin_utils::RouterResponse::builder()
                .errors(vec![crate::Error {
                    message: error.to_string(),
                    ..Default::default()
                }])
                .build()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR))
        });

        Box::pin(fut)
    }
}

pub struct PluggableRouterServiceBuilder {
    schema: Arc<Schema>,
    buffer: usize,
    plugins: Vec<Box<dyn DynPlugin>>,
    subgraph_services: Vec<(
        String,
        BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    )>,
}

impl PluggableRouterServiceBuilder {
    pub fn new(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            buffer: DEFAULT_BUFFER_SIZE,
            plugins: Default::default(),
            subgraph_services: Default::default(),
        }
    }

    pub fn with_plugin<E: DynPlugin + Plugin>(
        mut self,
        plugin: E,
    ) -> PluggableRouterServiceBuilder {
        self.plugins.push(Box::new(plugin));
        self
    }

    pub fn with_dyn_plugin(mut self, plugin: Box<dyn DynPlugin>) -> PluggableRouterServiceBuilder {
        self.plugins.push(plugin);
        self
    }

    // Consume the builder and retrieve its plugins
    pub fn plugins(self) -> Vec<Box<dyn DynPlugin>> {
        self.plugins
    }

    pub fn with_subgraph_service<
        S: Service<
                SubgraphRequest,
                Response = SubgraphResponse,
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
        self.subgraph_services
            .push((name.to_string(), service.boxed()));
        self
    }

    pub async fn build(
        mut self,
    ) -> (
        BoxCloneService<RouterRequest, RouterResponse, BoxError>,
        Vec<Box<dyn DynPlugin>>,
    ) {
        // Note: The plugins are always applied in reverse, so that the
        // fold is applied in the correct sequence. We could reverse
        // the list of plugins, but we want them back in the original
        // order at the end of this function. Instead, we reverse the
        // various iterators that we create for folding and leave
        // the plugins in their original order.

        //QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let plan_cache_limit = std::env::var("ROUTER_PLAN_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let query_planner_service = ServiceBuilder::new().buffer(self.buffer).service(
            self.plugins.iter_mut().rev().fold(
                CachingQueryPlanner::new(
                    RouterBridgeQueryPlanner::new(self.schema.clone()),
                    plan_cache_limit,
                )
                .boxed(),
                |acc, e| e.query_planning_service(acc),
            ),
        );

        // SubgraphService takes a SubgraphRequest and outputs a RouterResponse
        let subgraphs = self
            .subgraph_services
            .into_iter()
            .map(|(name, s)| {
                let service = self
                    .plugins
                    .iter_mut()
                    .rev()
                    .fold(s, |acc, e| e.subgraph_service(&name, acc));

                let service = ServiceBuilder::new().buffer(self.buffer).service(service);

                (name.clone(), service)
            })
            .collect();

        // ExecutionService takes a PlannedRequest and outputs a RouterResponse
        // NB: Cannot use .buffer() here or the code won't compile...
        let execution_service = Buffer::new(
            ServiceBuilder::new()
                .layer(ForbidHttpGetMutationsLayer::default())
                .service(
                    self.plugins.iter_mut().rev().fold(
                        ExecutionService::builder()
                            .schema(self.schema.clone())
                            .subgraph_services(subgraphs)
                            .build()
                            .boxed(),
                        |acc, e| e.execution_service(acc),
                    ),
                )
                .boxed(),
            self.buffer,
        );

        let query_cache_limit = std::env::var("ROUTER_QUERY_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        let query_cache = Arc::new(QueryCache::new(query_cache_limit, self.schema.clone()));

        // NaiveIntrospection instantiation can potentially block for some time
        // We don't need to use the api schema here because on the deno side we always convert to API schema
        let naive_introspection = {
            let schema = self.schema.clone();
            tokio::task::spawn_blocking(move || NaiveIntrospection::from_schema(&schema))
                .await
                .expect("NaiveIntrospection instantiation panicked")
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

        // Router service takes a graphql::Request and outputs a graphql::Response
        // NB: Cannot use .buffer() here or the code won't compile...
        let router_service = Buffer::new(
            ServiceBuilder::new()
                .layer(APQLayer::default())
                .layer(EnsureQueryPresence::default())
                .service(
                    self.plugins.iter_mut().rev().fold(
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
                )
                .boxed(),
            self.buffer,
        );

        (router_service.boxed_clone(), self.plugins)
    }
}
