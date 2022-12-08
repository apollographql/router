//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::stream::StreamExt;
use futures::TryFutureExt;
use http::StatusCode;
use indexmap::IndexMap;
use multimap::MultiMap;
use opentelemetry::trace::SpanKind;
use tower::util::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing_futures::Instrument;

use super::layers::apq::APQLayer;
use super::layers::content_negociation;
use super::layers::ensure_query_presence::EnsureQueryPresence;
use super::new_service::ServiceFactory;
use super::subgraph_service::MakeSubgraphService;
use super::subgraph_service::SubgraphCreator;
use super::ExecutionCreator;
use super::ExecutionServiceFactory;
use super::QueryPlannerContent;
use crate::cache::DeduplicatingCache;
use crate::error::CacheResolverError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::introspection::Introspection;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::plugin::DynPlugin;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::supergraph;
use crate::Configuration;
use crate::Context;
use crate::Endpoint;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::ListenAddr;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::Schema;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct SupergraphService<ExecutionFactory> {
    execution_service_factory: ExecutionFactory,
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    schema: Arc<Schema>,
}

#[buildstructor::buildstructor]
impl<ExecutionFactory> SupergraphService<ExecutionFactory> {
    #[builder]
    pub(crate) fn new(
        query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
        execution_service_factory: ExecutionFactory,
        schema: Arc<Schema>,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service_factory,
            schema,
        }
    }
}

impl<ExecutionFactory> Service<SupergraphRequest> for SupergraphService<ExecutionFactory>
where
    ExecutionFactory: ExecutionServiceFactory,
{
    type Response = SupergraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.query_planner_service
            .poll_ready(cx)
            .map_err(|err| err.into())
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        // Consume our cloned services and allow ownership to be transferred to the async block.
        let clone = self.query_planner_service.clone();

        let planning = std::mem::replace(&mut self.query_planner_service, clone);
        let execution = self.execution_service_factory.create();

        let schema = self.schema.clone();

        let context_cloned = req.context.clone();
        let fut =
            service_call(planning, execution, schema, req).or_else(|error: BoxError| async move {
                let errors = vec![crate::error::Error {
                    message: error.to_string(),
                    extensions: serde_json_bytes::json!({
                        "code": "INTERNAL_SERVER_ERROR",
                    })
                    .as_object()
                    .unwrap()
                    .to_owned(),
                    ..Default::default()
                }];

                Ok(SupergraphResponse::builder()
                    .errors(errors)
                    .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                    .context(context_cloned)
                    .build()
                    .expect("building a response like this should not fail"))
            });

        Box::pin(fut)
    }
}

async fn service_call<ExecutionService>(
    planning: CachingQueryPlanner<BridgeQueryPlanner>,
    execution: ExecutionService,
    schema: Arc<Schema>,
    req: SupergraphRequest,
) -> Result<SupergraphResponse, BoxError>
where
    ExecutionService:
        Service<ExecutionRequest, Response = ExecutionResponse, Error = BoxError> + Send,
{
    let context = req.context;
    let body = req.supergraph_request.body();
    let variables = body.variables.clone();
    let QueryPlannerResponse {
        content,
        context,
        errors,
    } = match plan_query(planning, body, context.clone()).await {
        Ok(resp) => resp,
        Err(err) => match err.into_graphql_errors() {
            Ok(gql_errors) => {
                return Ok(SupergraphResponse::builder()
                    .context(context)
                    .errors(gql_errors)
                    .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
                    .build()
                    .expect("this response build must not fail"));
            }
            Err(err) => return Err(err.into()),
        },
    };

    if !errors.is_empty() {
        return Ok(SupergraphResponse::builder()
            .context(context)
            .errors(errors)
            .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
            .build()
            .expect("this response build must not fail"));
    }

    match content {
        Some(QueryPlannerContent::Introspection { response }) => Ok(
            SupergraphResponse::new_from_graphql_response(*response, context),
        ),
        Some(QueryPlannerContent::IntrospectionDisabled) => {
            let mut response = SupergraphResponse::new_from_graphql_response(
                graphql::Response::builder()
                    .errors(vec![crate::error::Error::builder()
                        .message(String::from("introspection has been disabled"))
                        .build()])
                    .build(),
                context,
            );
            *response.response.status_mut() = StatusCode::BAD_REQUEST;
            Ok(response)
        }

        Some(QueryPlannerContent::Plan { plan }) => {
            let operation_name = body.operation_name.clone();
            let is_deferred = plan.is_deferred(operation_name.as_deref(), &variables);

            let accepts_multipart: bool = context
                .get("accepts-multipart")
                .unwrap_or_default()
                .unwrap_or_default();

            if is_deferred && !accepts_multipart {
                let mut response = SupergraphResponse::new_from_graphql_response(graphql::Response::builder()
                    .errors(vec![crate::error::Error::builder()
                        .message(String::from("the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed; deferSpec=20220824'"))
                        .build()])
                    .build(), context);
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                Ok(response)
            } else if let Some(err) = plan.query.validate_variables(body, &schema).err() {
                let mut res = SupergraphResponse::new_from_graphql_response(err, context);
                *res.response.status_mut() = StatusCode::BAD_REQUEST;
                Ok(res)
            } else {
                let execution_response = execution
                    .oneshot(
                        ExecutionRequest::builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan.clone())
                            .context(context)
                            .build(),
                    )
                    .await?;

                let ExecutionResponse { response, context } = execution_response;

                let (parts, response_stream) = response.into_parts();

                Ok(SupergraphResponse {
                    context,
                    response: http::Response::from_parts(
                        parts,
                        response_stream.in_current_span().boxed(),
                    ),
                })
            }
        }
        // This should never happen because if we have an empty query plan we should have error in errors vec
        None => Err(BoxError::from("cannot compute a query plan")),
    }
}

async fn plan_query(
    mut planning: CachingQueryPlanner<BridgeQueryPlanner>,
    body: &graphql::Request,
    context: Context,
) -> Result<QueryPlannerResponse, CacheResolverError> {
    planning
        .call(
            QueryPlannerRequest::builder()
                .query(
                    body.query
                        .clone()
                        .expect("the query presence was already checked by a plugin"),
                )
                .and_operation_name(body.operation_name.clone())
                .context(context)
                .build(),
        )
        .instrument(tracing::info_span!("query_planning",
            graphql.document = body.query.clone().expect("the query presence was already checked by a plugin").as_str(),
            graphql.operation.name = body.operation_name.clone().unwrap_or_default().as_str(),
            "otel.kind" = %SpanKind::Internal
        ))
        .await
}

/// Builder which generates a plugin pipeline.
///
/// This is at the heart of the delegation of responsibility model for the router. A schema,
/// collection of plugins, collection of subgraph services are assembled to generate a
/// [`tower::util::BoxCloneService`] capable of processing a router request
/// through the entire stack to return a response.
pub(crate) struct PluggableSupergraphServiceBuilder {
    schema: Arc<Schema>,
    plugins: Plugins,
    subgraph_services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
    configuration: Option<Arc<Configuration>>,
}

impl PluggableSupergraphServiceBuilder {
    pub(crate) fn new(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            plugins: Default::default(),
            subgraph_services: Default::default(),
            configuration: None,
        }
    }

    pub(crate) fn with_dyn_plugin(
        mut self,
        plugin_name: String,
        plugin: Box<dyn DynPlugin>,
    ) -> PluggableSupergraphServiceBuilder {
        self.plugins.insert(plugin_name, plugin);
        self
    }

    pub(crate) fn with_subgraph_service<S>(
        mut self,
        name: &str,
        service_maker: S,
    ) -> PluggableSupergraphServiceBuilder
    where
        S: MakeSubgraphService,
    {
        self.subgraph_services
            .push((name.to_string(), Arc::new(service_maker)));
        self
    }

    pub(crate) fn with_configuration(
        mut self,
        configuration: Arc<Configuration>,
    ) -> PluggableSupergraphServiceBuilder {
        self.configuration = Some(configuration);
        self
    }

    pub(crate) async fn build(self) -> Result<SupergraphCreator, crate::error::ServiceBuildError> {
        // Note: The plugins are always applied in reverse, so that the
        // fold is applied in the correct sequence. We could reverse
        // the list of plugins, but we want them back in the original
        // order at the end of this function. Instead, we reverse the
        // various iterators that we create for folding and leave
        // the plugins in their original order.

        let configuration = self.configuration.unwrap_or_default();

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(&configuration).await))
        } else {
            None
        };

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner =
            BridgeQueryPlanner::new(self.schema.clone(), introspection, configuration.clone())
                .await
                .map_err(ServiceBuildError::QueryPlannerError)?;
        let query_planner_service = CachingQueryPlanner::new(
            bridge_query_planner,
            self.schema.schema_id.clone(),
            &configuration.supergraph.query_planning,
        )
        .await;

        let plugins = Arc::new(self.plugins);

        let subgraph_creator = Arc::new(SubgraphCreator::new(
            self.subgraph_services,
            plugins.clone(),
        ));

        let apq_layer = APQLayer::with_cache(
            DeduplicatingCache::from_configuration(
                &configuration.supergraph.apq.experimental_cache,
                "APQ",
            )
            .await,
        );

        Ok(SupergraphCreator {
            query_planner_service,
            subgraph_creator,
            apq_layer,
            schema: self.schema,
            plugins,
        })
    }
}

/// Factory for creating a RouterService
///
/// Instances of this traits are used by the HTTP server to generate a new
/// RouterService on each request
pub(crate) trait SupergraphFactory:
    ServiceFactory<supergraph::Request, Service = Self::SupergraphService>
    + Clone
    + Send
    + Sync
    + 'static
{
    type SupergraphService: Service<
            supergraph::Request,
            Response = supergraph::Response,
            Error = BoxError,
            Future = Self::Future,
        > + Send;
    type Future: Send;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct SupergraphCreator {
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    subgraph_creator: Arc<SubgraphCreator>,
    apq_layer: APQLayer,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
}

pub(crate) trait StuffThatHasPlugins {
    fn plugins(&self) -> Arc<Plugins>;
}

impl StuffThatHasPlugins for SupergraphCreator {
    fn plugins(&self) -> Arc<Plugins> {
        self.plugins.clone()
    }
}

impl ServiceFactory<supergraph::Request> for SupergraphCreator {
    type Service = supergraph::BoxService;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl SupergraphCreator {
    pub(crate) fn make(
        &self,
    ) -> impl Service<
        supergraph::Request,
        Response = supergraph::Response,
        Error = BoxError,
        Future = BoxFuture<'static, supergraph::ServiceResult>,
    > + Send {
        let supergraph_service = SupergraphService::builder()
            .query_planner_service(self.query_planner_service.clone())
            .execution_service_factory(ExecutionCreator {
                schema: self.schema.clone(),
                plugins: self.plugins.clone(),
                subgraph_creator: self.subgraph_creator.clone(),
            })
            .schema(self.schema.clone())
            .build();

        let supergraph_service = match self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
            .and_then(|plugin| plugin.1.as_any().downcast_ref::<TrafficShaping>())
        {
            Some(shaping) => Either::A(shaping.supergraph_service_internal(supergraph_service)),
            None => Either::B(supergraph_service),
        };

        ServiceBuilder::new()
            .layer(self.apq_layer.clone())
            .layer(content_negociation::SupergraphLayer {})
            .layer(EnsureQueryPresence::default())
            .service(
                self.plugins
                    .iter()
                    .rev()
                    .fold(supergraph_service.boxed(), |acc, (_, e)| {
                        e.supergraph_service(acc)
                    }),
            )
    }

    /// Create a test service.
    #[cfg(test)]
    pub(crate) async fn for_tests(
        supergraph_service: MockSupergraphService,
    ) -> MockSupergraphCreator {
        MockSupergraphCreator::new(supergraph_service).await
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct MockSupergraphCreator {
    supergraph_service: MockSupergraphService,
    plugins: Arc<Plugins>,
}

#[cfg(test)]
impl MockSupergraphCreator {
    pub(crate) async fn new(supergraph_service: MockSupergraphService) -> Self {
        let canned_schema = include_str!("../../testing_schema.graphql");
        let configuration = Configuration::builder().build().unwrap();

        use crate::router_factory::create_plugins;
        let plugins = create_plugins(
            &configuration,
            &Schema::parse(canned_schema, &configuration).unwrap(),
            None,
        )
        .await
        .unwrap()
        .into_iter()
        .collect();

        Self {
            supergraph_service,
            plugins: Arc::new(plugins),
        }
    }
}

#[cfg(test)]
impl StuffThatHasPlugins for MockSupergraphCreator {
    fn plugins(&self) -> Arc<Plugins> {
        self.plugins.clone()
    }
}

#[cfg(test)]
impl ServiceFactory<supergraph::Request> for MockSupergraphCreator {
    type Service = supergraph::BoxService;
    fn create(&self) -> Self::Service {
        self.supergraph_service.clone().boxed()
    }
}
