//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use futures::Stream;
use futures::TryFutureExt;
use http::StatusCode;
use indexmap::IndexMap;
use lazy_static::__Deref;
use tower::buffer::Buffer;
use tower::util::BoxCloneService;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing_futures::Instrument;

use super::QueryPlannerContent;
use crate::cache::storage::CacheStorage;
use crate::cache::DedupCache;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::graphql::Response;
use crate::introspection::Introspection;
use crate::layers::ServiceBuilderExt;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::plugin::DynPlugin;
use crate::plugin::Plugin;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::QueryPlanOptions;
use crate::services::execution_service::ExecutionService;
use crate::services::layers::allow_only_http_post_mutations::AllowOnlyHttpPostMutationsLayer;
use crate::services::layers::apq::APQLayer;
use crate::services::layers::ensure_query_presence::EnsureQueryPresence;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::Schema;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub struct RouterService<QueryPlannerService, ExecutionService> {
    query_planner_service: QueryPlannerService,
    query_execution_service: ExecutionService,
    ready_query_planner_service: Option<QueryPlannerService>,
    ready_query_execution_service: Option<ExecutionService>,
    schema: Arc<Schema>,
}

#[buildstructor::buildstructor]
impl<QueryPlannerService, ExecutionService> RouterService<QueryPlannerService, ExecutionService> {
    #[builder]
    pub fn new(
        query_planner_service: QueryPlannerService,
        query_execution_service: ExecutionService,
        schema: Arc<Schema>,
    ) -> RouterService<QueryPlannerService, ExecutionService> {
        RouterService {
            query_planner_service,
            query_execution_service,
            ready_query_planner_service: None,
            ready_query_execution_service: None,
            schema,
        }
    }
}

impl<ResponseStream, QueryPlannerService, ExecutionService> Service<RouterRequest>
    for RouterService<QueryPlannerService, ExecutionService>
where
    QueryPlannerService: Service<QueryPlannerRequest, Response = QueryPlannerResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    ExecutionService: Service<ExecutionRequest, Response = ExecutionResponse<ResponseStream>, Error = BoxError>
        + Clone
        + Send
        + 'static,
    QueryPlannerService::Future: Send + 'static,
    ExecutionService::Future: Send + 'static,
    ResponseStream: Stream<Item = Response> + Send + 'static,
{
    type Response = RouterResponse<BoxStream<'static, Response>>;
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

        let context_cloned = req.context.clone();
        let fut = async move {
            let context = req.context;
            let body = req.originating_request.body();
            let variables = body.variables.clone();
            let QueryPlannerResponse { content, context } = planning
                .call(
                    QueryPlannerRequest::builder()
                        .originating_request(req.originating_request.clone())
                        .query_plan_options(QueryPlanOptions::default())
                        .context(context)
                        .build(),
                )
                .await?;

            match content {
                QueryPlannerContent::Introspection { response } => {
                    return Ok(
                        RouterResponse::new_from_graphql_response(*response, context).boxed(),
                    );
                }
                QueryPlannerContent::IntrospectionDisabled => {
                    let mut resp = http::Response::new(once(ready(
                        graphql::Response::builder()
                            .errors(vec![crate::error::Error::builder()
                                .message(String::from("introspection has been disabled"))
                                .build()])
                            .build(),
                    )));
                    *resp.status_mut() = StatusCode::BAD_REQUEST;

                    return Ok(RouterResponse {
                        response: resp.into(),
                        context,
                    }
                    .boxed());
                }
                QueryPlannerContent::Plan { query, plan } => {
                    if let Some(err) = query.validate_variables(body, &schema).err() {
                        Ok(RouterResponse::new_from_graphql_response(err, context).boxed())
                    } else {
                        let operation_name = body.operation_name.clone();

                        let ExecutionResponse { response, context }: ExecutionResponse<
                            ResponseStream,
                        > = execution
                            .call(
                                ExecutionRequest::builder()
                                    .originating_request(req.originating_request.clone())
                                    .query_plan(plan)
                                    .context(context)
                                    .build(),
                            )
                            .await?;

                        let (parts, response_stream) = http::Response::from(response).into_parts();
                        Ok(RouterResponse {
                            context,
                            response: http::Response::from_parts(
                                parts,
                                response_stream
                                    .map(move |mut response: Response| {
                                        tracing::debug_span!("format_response").in_scope(|| {
                                            query.format_response(
                                                &mut response,
                                                operation_name.as_deref(),
                                                variables.clone(),
                                                schema.api_schema(),
                                            )
                                        });
                                        response
                                    })
                                    .in_current_span(),
                            )
                            .into(),
                        }
                        .boxed())
                    }
                }
            }
        }
        .or_else(|error: BoxError| async move {
            let errors = vec![crate::error::Error {
                message: error.to_string(),
                ..Default::default()
            }];
            let status_code = match error.downcast_ref::<crate::error::CacheResolverError>() {
                Some(crate::error::CacheResolverError::RetrievalError(retrieval_error))
                    if matches!(
                        retrieval_error.deref(),
                        QueryPlannerError::SpecError(_)
                            | QueryPlannerError::SchemaValidationErrors(_)
                    ) =>
                {
                    StatusCode::BAD_REQUEST
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };

            Ok(RouterResponse::builder()
                .errors(errors)
                .status_code(status_code)
                .context(context_cloned)
                .build()
                .expect("building a response like this should not fail")
                .boxed())
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
            BoxCloneService<RouterRequest, RouterResponse<BoxStream<'static, Response>>, BoxError>,
            Plugins,
        ),
        crate::error::ServiceBuildError,
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

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner = BridgeQueryPlanner::new(self.schema.clone(), introspection)
            .await
            .map_err(ServiceBuildError::QueryPlannerError)?;
        let query_planner_service = ServiceBuilder::new().buffered().service(
            self.plugins.iter_mut().rev().fold(
                CachingQueryPlanner::new(bridge_query_planner, plan_cache_limit)
                    .await
                    .boxed(),
                |acc, (_, e)| e.query_planning_service(acc),
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
                    .fold(s, |acc, (_, e)| e.subgraph_service(&name, acc));

                let service = ServiceBuilder::new().buffered().service(service);

                (name.clone(), service)
            })
            .collect();

        // ExecutionService takes a PlannedRequest and outputs a RouterResponse
        // NB: Cannot use .buffer() here or the code won't compile...
        let execution_service = Buffer::new(
            ServiceBuilder::new()
                .layer(AllowOnlyHttpPostMutationsLayer::default())
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

        let apq = APQLayer::with_cache(DedupCache::new(512).await);

        // Router service takes a crate::Request and outputs a crate::Response
        // NB: Cannot use .buffer() here or the code won't compile...
        let router_service = Buffer::new(
            ServiceBuilder::new()
                .layer(apq)
                .layer(EnsureQueryPresence::default())
                .service(
                    self.plugins.iter_mut().rev().fold(
                        RouterService::builder()
                            .query_planner_service(query_planner_service)
                            .query_execution_service(execution_service)
                            .schema(self.schema)
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
