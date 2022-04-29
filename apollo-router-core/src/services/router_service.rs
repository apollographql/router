//! Implements the router phase of the request lifecycle.

use crate::apq::APQLayer;
use crate::ensure_query_presence::EnsureQueryPresence;
use crate::forbid_http_get_mutations::ForbidHttpGetMutationsLayer;
use crate::services::execution_service::ExecutionService;
use crate::{
    BridgeQueryPlanner, CachingQueryPlanner, DynPlugin, ExecutionRequest, ExecutionResponse,
    Introspection, Plugin, QueryCache, QueryPlannerRequest, QueryPlannerResponse, ResponseBody,
    RouterRequest, RouterResponse, Schema, ServiceBuildError, ServiceBuilderExt, SubgraphRequest,
    SubgraphResponse, DEFAULT_BUFFER_SIZE,
};
use futures::{future::BoxFuture, TryFutureExt};
use http::StatusCode;
use indexmap::IndexMap;
use std::sync::Arc;
use std::task::Poll;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;
use typed_builder::TypedBuilder;

pub type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
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
    introspection: Option<Arc<Introspection>>,
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
        let naive_introspection = self.introspection.clone();

        let schema = self.schema.clone();
        let query_cache = self.query_cache.clone();

        let context_cloned = req.context.clone();
        let fut =
            async move {
                // Check if we already have the query in the known introspection queries
                if let Some(naive_introspection) = naive_introspection.as_ref() {
                    if let Some(response) =
                        naive_introspection
                            .get(req.originating_request.body().query.as_ref().expect(
                                "apollo.ensure-query-is-present has checked this already; qed",
                            ))
                            .await
                    {
                        return Ok(RouterResponse {
                            response: http::Response::new(ResponseBody::GraphQL(response)).into(),
                            context: req.context,
                        });
                    }
                }

                let context = req.context;
                let body = req.originating_request.body();
                let variables = body.variables.clone();
                let query = query_cache
                    .get(
                        body.query
                            .as_ref()
                            .expect("apollo.ensure-query-is-present has checked this already; qed")
                            .as_str(),
                    )
                    .await;

                // Check if it's an introspection query
                if let Some(current_query) = query.as_ref().filter(|q| q.contains_introspection()) {
                    match naive_introspection.as_ref() {
                        Some(naive_introspection) => {
                            match naive_introspection
                                .execute(schema.as_str(), current_query.as_str())
                                .await
                            {
                                Ok(resp) => {
                                    return Ok(RouterResponse {
                                        response: http::Response::new(ResponseBody::GraphQL(resp))
                                            .into(),
                                        context,
                                    });
                                }
                                Err(err) => return Err(BoxError::from(err)),
                            }
                        }
                        None => {
                            let mut resp = http::Response::new(ResponseBody::GraphQL(
                                crate::Response::builder()
                                    .errors(vec![crate::Error::builder()
                                        .message(String::from("introspection has been disabled"))
                                        .build()])
                                    .build(),
                            ));
                            *resp.status_mut() = StatusCode::BAD_REQUEST;

                            return Ok(RouterResponse {
                                response: resp.into(),
                                context,
                            });
                        }
                    }
                }

                if let Some(err) = query
                    .as_ref()
                    .and_then(|q| q.validate_variables(body, &schema).err())
                {
                    Ok(RouterResponse {
                        response: http::Response::new(ResponseBody::GraphQL(err)).into(),
                        context,
                    })
                } else {
                    let operation_name = body.operation_name.clone();
                    let planned_query = planning
                        .call(
                            QueryPlannerRequest::builder()
                                .originating_request(req.originating_request.clone())
                                .context(context)
                                .build(),
                        )
                        .await?;
                    let mut response = execution
                        .call(
                            ExecutionRequest::builder()
                                .originating_request(req.originating_request.clone())
                                .query_plan(planned_query.query_plan)
                                .context(planned_query.context)
                                .build(),
                        )
                        .await?;

                    if let Some(query) = query {
                        tracing::debug_span!("format_response").in_scope(|| {
                            query.format_response(
                                response.response.body_mut(),
                                operation_name.as_deref(),
                                (*variables).clone(),
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
                let errors = vec![crate::Error {
                    message: error.to_string(),
                    ..Default::default()
                }];
                RouterResponse::builder()
                    .errors(errors)
                    .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                    .context(context_cloned)
                    .build()
            });

        Box::pin(fut)
    }
}

/// Builder which generates a plugin pipeline.
///
/// This is at the heart of the delegation of responsibility model for the router. A schema,
/// collection of plugins, collection of subgraph services are assembled to generate a
/// [`BoxCloneService`] capable of processing a router request through the entire stack to return a
/// response.
pub struct PluggableRouterServiceBuilder {
    schema: Arc<Schema>,
    plugins: Plugins,
    subgraph_services: Vec<(
        String,
        BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    )>,
    introspection: bool,
}

impl PluggableRouterServiceBuilder {
    pub fn new(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            plugins: Default::default(),
            subgraph_services: Default::default(),
            introspection: false,
        }
    }

    pub fn with_plugin<E: DynPlugin + Plugin>(
        mut self,
        plugin_name: String,
        plugin: E,
    ) -> PluggableRouterServiceBuilder {
        self.plugins.insert(plugin_name, Box::new(plugin));
        self
    }

    pub fn with_dyn_plugin(
        mut self,
        plugin_name: String,
        plugin: Box<dyn DynPlugin>,
    ) -> PluggableRouterServiceBuilder {
        self.plugins.insert(plugin_name, plugin);
        self
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

    pub fn with_naive_introspection(mut self) -> PluggableRouterServiceBuilder {
        self.introspection = true;
        self
    }

    pub async fn build(
        mut self,
    ) -> Result<
        (
            BoxCloneService<RouterRequest, RouterResponse, BoxError>,
            Plugins,
        ),
        crate::ServiceBuildError,
    > {
        // Note: The plugins are always applied in reverse, so that the
        // fold is applied in the correct sequence. We could reverse
        // the list of plugins, but we want them back in the original
        // order at the end of this function. Instead, we reverse the
        // various iterators that we create for folding and leave
        // the plugins in their original order.

        let plan_cache_limit = std::env::var("ROUTER_PLAN_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest

        let bridge_query_planner = BridgeQueryPlanner::new(self.schema.clone())
            .await
            .map_err(ServiceBuildError::QueryPlannerError)?;
        let query_planner_service =
            ServiceBuilder::new()
                .buffered()
                .service(self.plugins.iter_mut().rev().fold(
                    CachingQueryPlanner::new(bridge_query_planner, plan_cache_limit).boxed(),
                    |acc, (_, e)| e.query_planning_service(acc),
                ));

        // SubgraphService takes a SubgraphRequest and outputs a RouterResponse
        let subgraphs = self
            .subgraph_services
            .into_iter()
            .map(|(name, s)| {
                let service = self
                    .plugins
                    .iter_mut()
                    .rev()
                    .fold(s, |acc, (_, e)| e.subgraph_service(&name, acc));

                let service = ServiceBuilder::new().buffered().service(service);

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
                        |acc, (_, e)| e.execution_service(acc),
                    ),
                )
                .boxed(),
            DEFAULT_BUFFER_SIZE,
        );

        let query_cache_limit = std::env::var("ROUTER_QUERY_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        let query_cache = Arc::new(QueryCache::new(query_cache_limit, self.schema.clone()));

        let introspection = if self.introspection {
            // Introspection instantiation can potentially block for some time
            // We don't need to use the api schema here because on the deno side we always convert to API schema

            let schema = self.schema.clone();
            Some(Arc::new(
                tokio::task::spawn_blocking(move || Introspection::from_schema(&schema))
                    .await
                    .expect("Introspection instantiation panicked"),
            ))
        } else {
            None
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
                            .introspection(introspection)
                            .build()
                            .boxed(),
                        |acc, (_, e)| e.router_service(acc),
                    ),
                )
                .boxed(),
            DEFAULT_BUFFER_SIZE,
        );

        Ok((router_service.boxed_clone(), self.plugins))
    }
}
