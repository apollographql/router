//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::ApolloCompiler;
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use futures::TryFutureExt;
use http::StatusCode;
use indexmap::IndexMap;
use router_bridge::planner::Planner;
use tokio::sync::Mutex;
use tower::util::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing_futures::Instrument;

use super::execution;
use super::layers::content_negociation;
use super::layers::query_analysis::QueryAnalysisLayer;
use super::new_service::ServiceFactory;
use super::router::ClientRequestAccepts;
use super::subgraph_service::MakeSubgraphService;
use super::subgraph_service::SubgraphServiceFactory;
use super::ExecutionServiceFactory;
use super::QueryPlannerContent;
use crate::error::CacheResolverError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::plugin::DynPlugin;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::QueryPlanResult;
use crate::services::query_planner;
use crate::services::supergraph;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::QueryPlannerResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;

#[cfg(test)]
mod tests;

pub(crate) const QUERY_PLANNING_SPAN_NAME: &str = "query_planning";

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct SupergraphService {
    execution_service_factory: ExecutionServiceFactory,
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    schema: Arc<Schema>,
}

#[buildstructor::buildstructor]
impl SupergraphService {
    #[builder]
    pub(crate) fn new(
        query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
        execution_service_factory: ExecutionServiceFactory,
        schema: Arc<Schema>,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service_factory,
            schema,
        }
    }
}

impl Service<SupergraphRequest> for SupergraphService {
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

async fn service_call(
    planning: CachingQueryPlanner<BridgeQueryPlanner>,
    execution: execution::BoxService,
    schema: Arc<Schema>,
    req: SupergraphRequest,
) -> Result<SupergraphResponse, BoxError> {
    let context = req.context;
    let body = req.supergraph_request.body();
    let variables = body.variables.clone();
    let QueryPlannerResponse {
        content,
        context,
        errors,
    } = match plan_query(
        planning,
        req.compiler,
        body.operation_name.clone(),
        context.clone(),
        schema.clone(),
        req.supergraph_request
            .body()
            .query
            .clone()
            .expect("query presence was checked before"),
    )
    .await
    {
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
                        .extension_code("INTROSPECTION_DISABLED")
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
            let is_subscription = plan.is_subscription(operation_name.as_deref());

            let ClientRequestAccepts {
                multipart_defer: accepts_multipart_defer,
                multipart_subscription: accepts_multipart_subscription,
                ..
            } = context
                .private_entries
                .lock()
                .get()
                .cloned()
                .unwrap_or_default();

            if (is_deferred || is_subscription)
                && !accepts_multipart_defer
                && !accepts_multipart_subscription
            {
                let (error_message, error_code) = if is_deferred {
                    (String::from("the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed; deferSpec=20220824'"), "DEFER_BAD_HEADER")
                } else {
                    (String::from("the router received a query with a subscription but the client does not accept multipart/mixed HTTP responses. To enable subscription support, add the HTTP header 'Accept: multipart/mixed; boundary=graphql; subscriptionSpec=1.0'"), "SUBSCRIPTION_BAD_HEADER")
                };
                let mut response = SupergraphResponse::new_from_graphql_response(
                    graphql::Response::builder()
                        .errors(vec![crate::error::Error::builder()
                            .message(error_message)
                            .extension_code(error_code)
                            .build()])
                        .build(),
                    context,
                );
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                Ok(response)
            } else if let Some(err) = plan.query.validate_variables(body, &schema).err() {
                let mut res = SupergraphResponse::new_from_graphql_response(err, context);
                *res.response.status_mut() = StatusCode::BAD_REQUEST;
                Ok(res)
            } else {
                let execution_response = execution
                    .oneshot(
                        ExecutionRequest::internal_builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan.clone())
                            .context(context)
                            .build()
                            .await,
                    )
                    .await?;

                let ExecutionResponse { response, context } = execution_response;

                let (parts, response_stream) = response.into_parts();

                Ok(SupergraphResponse {
                    context,
                    response: http::Response::from_parts(parts, response_stream.boxed()),
                })
            }
        }
        // This should never happen because if we have an empty query plan we should have error in errors vec
        None => Err(BoxError::from("cannot compute a query plan")),
    }
}

async fn plan_query(
    mut planning: CachingQueryPlanner<BridgeQueryPlanner>,
    compiler: Option<Arc<Mutex<ApolloCompiler>>>,
    operation_name: Option<String>,
    context: Context,
    schema: Arc<Schema>,
    query_str: String,
) -> Result<QueryPlannerResponse, CacheResolverError> {
    let compiler = match compiler {
        None =>
        // TODO[igni]: no
        {
            Arc::new(Mutex::new(
                QueryAnalysisLayer::new(schema, Default::default())
                    .await
                    .make_compiler(&query_str)
                    .0,
            ))
        }
        Some(c) => c,
    };

    planning
        .call(
            query_planner::CachingRequest::builder()
                .query(query_str)
                .and_operation_name(operation_name)
                .compiler(compiler)
                .context(context)
                .build(),
        )
        .instrument(tracing::info_span!(
            QUERY_PLANNING_SPAN_NAME,
            "otel.kind" = "INTERNAL"
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
    plugins: Plugins,
    subgraph_services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
    configuration: Option<Arc<Configuration>>,
    planner: BridgeQueryPlanner,
}

impl PluggableSupergraphServiceBuilder {
    pub(crate) fn new(planner: BridgeQueryPlanner) -> Self {
        Self {
            plugins: Default::default(),
            subgraph_services: Default::default(),
            configuration: None,
            planner,
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
        let configuration = self.configuration.unwrap_or_default();

        let schema = self.planner.schema();
        let query_planner_service = CachingQueryPlanner::new(
            self.planner,
            schema.clone(),
            &configuration,
            IndexMap::new(),
        )
        .await;

        let mut plugins = self.plugins;
        // Activate the telemetry plugin.
        // We must NOT fail to go live with the new router from this point as the telemetry plugin activate interacts with globals.
        for (_, plugin) in plugins.iter_mut() {
            if let Some(telemetry) = plugin.as_any_mut().downcast_mut::<Telemetry>() {
                telemetry.activate();
            }
        }

        let plugins = Arc::new(plugins);

        let subgraph_service_factory = Arc::new(SubgraphServiceFactory::new(
            self.subgraph_services,
            plugins.clone(),
        ));

        Ok(SupergraphCreator {
            query_planner_service,
            subgraph_service_factory,
            schema,
            plugins,
        })
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct SupergraphCreator {
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    subgraph_service_factory: Arc<SubgraphServiceFactory>,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
}

pub(crate) trait HasPlugins {
    fn plugins(&self) -> Arc<Plugins>;
}

impl HasPlugins for SupergraphCreator {
    fn plugins(&self) -> Arc<Plugins> {
        self.plugins.clone()
    }
}

pub(crate) trait HasSchema {
    fn schema(&self) -> Arc<Schema>;
}

impl HasSchema for SupergraphCreator {
    fn schema(&self) -> Arc<Schema> {
        Arc::clone(&self.schema)
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
            .execution_service_factory(ExecutionServiceFactory {
                schema: self.schema.clone(),
                plugins: self.plugins.clone(),
                subgraph_service_factory: self.subgraph_service_factory.clone(),
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
            .layer(content_negociation::SupergraphLayer::default())
            .service(
                self.plugins
                    .iter()
                    .rev()
                    .fold(supergraph_service.boxed(), |acc, (_, e)| {
                        e.supergraph_service(acc)
                    }),
            )
    }

    pub(crate) async fn cache_keys(&self, count: usize) -> Vec<(String, Option<String>)> {
        self.query_planner_service.cache_keys(count).await
    }

    pub(crate) fn planner(&self) -> Arc<Planner<QueryPlanResult>> {
        self.query_planner_service.planner()
    }

    pub(crate) async fn warm_up_query_planner(
        &mut self,
        query_parser: &QueryAnalysisLayer,
        cache_keys: Vec<(String, Option<String>)>,
    ) {
        self.query_planner_service
            .warm_up(query_parser, cache_keys)
            .await
    }
}
