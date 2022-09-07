//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::stream::StreamExt;
use futures::TryFutureExt;
use http::header::ACCEPT;
use http::HeaderMap;
use http::StatusCode;
use indexmap::IndexMap;
use lazy_static::__Deref;
use mediatype::names::MIXED;
use mediatype::names::MULTIPART;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use multimap::MultiMap;
use opentelemetry::trace::SpanKind;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing_futures::Instrument;

use super::new_service::NewService;
use super::subgraph_service::MakeSubgraphService;
use super::subgraph_service::SubgraphCreator;
use super::ExecutionCreator;
use super::ExecutionServiceFactory;
use super::QueryPlannerContent;
use super::MULTIPART_DEFER_SPEC_PARAMETER;
use super::MULTIPART_DEFER_SPEC_VALUE;
use crate::cache::DeduplicatingCache;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::graphql::Response;
use crate::introspection::Introspection;
use crate::json_ext::ValueExt;
use crate::plugin::DynPlugin;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::response::IncrementalResponse;
use crate::router_factory::Endpoint;
use crate::router_factory::SupergraphServiceFactory;
use crate::services::layers::apq::APQLayer;
use crate::services::layers::ensure_query_presence::EnsureQueryPresence;
use crate::spec::Query;
use crate::Configuration;
use crate::Context;
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
    ready_query_planner_service: Option<CachingQueryPlanner<BridgeQueryPlanner>>,
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
            ready_query_planner_service: None,
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
        // We need to obtain references to two hot services for use in call.
        // The reason for us to clone here is that the async block needs to own the hot services,
        // and cloning will produce a cold service. Therefore cloning in `SupergraphService#call` is not
        // a valid course of action.
        self.ready_query_planner_service
            .get_or_insert_with(|| self.query_planner_service.clone())
            .poll_ready(cx)
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        // Consume our cloned services and allow ownership to be transferred to the async block.
        let planning = self.ready_query_planner_service.take().unwrap();
        let execution = self.execution_service_factory.new_service();

        let schema = self.schema.clone();

        let context_cloned = req.context.clone();
        let fut =
            service_call(planning, execution, schema, req).or_else(|error: BoxError| async move {
                let errors = vec![crate::error::Error {
                    message: error.to_string(),
                    ..Default::default()
                }];
                let status_code = match error.downcast_ref::<crate::error::CacheResolverError>() {
                    Some(crate::error::CacheResolverError::RetrievalError(retrieval_error))
                        if matches!(
                            retrieval_error.deref().downcast_ref::<QueryPlannerError>(),
                            Some(QueryPlannerError::SpecError(_))
                                | Some(QueryPlannerError::SchemaValidationErrors(_))
                        ) =>
                    {
                        StatusCode::BAD_REQUEST
                    }
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };

                Ok(SupergraphResponse::builder()
                    .errors(errors)
                    .status_code(status_code)
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
    let QueryPlannerResponse { content, context } = plan_query(planning, body, context).await?;

    match content {
        QueryPlannerContent::Introspection { response } => Ok(
            SupergraphResponse::new_from_graphql_response(*response, context),
        ),
        QueryPlannerContent::IntrospectionDisabled => {
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
        QueryPlannerContent::Plan { query, plan } => {
            let can_be_deferred = plan.root.contains_defer();

            if can_be_deferred && !accepts_multipart(req.supergraph_request.headers()) {
                let mut response = SupergraphResponse::new_from_graphql_response(graphql::Response::builder()
                    .errors(vec![crate::error::Error::builder()
                        .message(String::from("the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed; deferSpec=20220824'"))
                        .build()])
                    .build(), context);
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                Ok(response)
            } else if let Some(err) = query.validate_variables(body, &schema).err() {
                let mut res = SupergraphResponse::new_from_graphql_response(err, context);
                *res.response.status_mut() = StatusCode::BAD_REQUEST;
                Ok(res)
            } else {
                let operation_name = body.operation_name.clone();

                let execution_response = execution
                    .oneshot(
                        ExecutionRequest::builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan)
                            .context(context)
                            .build(),
                    )
                    .await?;

                process_execution_response(
                    execution_response,
                    query,
                    operation_name,
                    variables,
                    schema,
                    can_be_deferred,
                )
            }
        }
    }
}

async fn plan_query(
    mut planning: CachingQueryPlanner<BridgeQueryPlanner>,
    body: &graphql::Request,
    context: Context,
) -> Result<QueryPlannerResponse, BoxError> {
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

fn accepts_multipart(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| {
                    mime.as_ref()
                        .map(|mime| {
                            mime.ty == MULTIPART
                                && mime.subty == MIXED
                                && mime.get_param(
                                    mediatype::Name::new(MULTIPART_DEFER_SPEC_PARAMETER)
                                        .expect("valid name"),
                                ) == Some(
                                    mediatype::Value::new(MULTIPART_DEFER_SPEC_VALUE)
                                        .expect("valid value"),
                                )
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

fn process_execution_response(
    execution_response: ExecutionResponse,
    query: Arc<Query>,
    operation_name: Option<String>,
    variables: Map<ByteString, Value>,
    schema: Arc<Schema>,
    can_be_deferred: bool,
) -> Result<SupergraphResponse, BoxError> {
    let ExecutionResponse { response, context } = execution_response;

    let (parts, response_stream) = response.into_parts();

    let stream = response_stream.map(move |mut response: Response| {
        tracing::debug_span!("format_response").in_scope(|| {
            query.format_response(
                &mut response,
                operation_name.as_deref(),
                variables.clone(),
                schema.api_schema(),
            )
        });

        match (response.path.as_ref(), response.data.as_ref()) {
            (None, _) | (_, None) => {
                if can_be_deferred {
                    response.has_next = Some(true);
                }

                response
            }
            // if the deferred response specified a path, we must extract the
            //values matched by that path and create a separate response for
            //each of them.
            // While { "data": { "a": { "b": 1 } } } and { "data": { "b": 1 }, "path: ["a"] }
            // would merge in the same ways, some clients will generate code
            // that checks the specific type of the deferred response at that
            // path, instead of starting from the root object, so to support
            // this, we extract the value at that path.
            // In particular, that means that a deferred fragment in an object
            // under an array would generate one response par array element
            (Some(response_path), Some(response_data)) => {
                let mut sub_responses = Vec::new();
                response_data.select_values_and_paths(response_path, |path, value| {
                    sub_responses.push((path.clone(), value.clone()));
                });

                Response::builder()
                    .has_next(true)
                    .incremental(
                        sub_responses
                            .into_iter()
                            .map(move |(path, data)| {
                                IncrementalResponse::builder()
                                    .and_label(response.label.clone())
                                    .data(data)
                                    .path(path)
                                    .errors(response.errors.clone())
                                    .extensions(response.extensions.clone())
                                    .build()
                            })
                            .collect(),
                    )
                    .build()
            }
        }
    });

    Ok(SupergraphResponse {
        context,
        response: http::Response::from_parts(
            parts,
            if can_be_deferred {
                stream
                    .chain(once(ready(Response::builder().has_next(false).build())))
                    .left_stream()
            } else {
                stream.right_stream()
            }
            .in_current_span()
            .boxed(),
        ),
    })
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

    pub(crate) async fn build(self) -> Result<RouterCreator, crate::error::ServiceBuildError> {
        // Note: The plugins are always applied in reverse, so that the
        // fold is applied in the correct sequence. We could reverse
        // the list of plugins, but we want them back in the original
        // order at the end of this function. Instead, we reverse the
        // various iterators that we create for folding and leave
        // the plugins in their original order.

        let configuration = self.configuration.unwrap_or_default();

        let plan_cache_limit = std::env::var("ROUTER_PLAN_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);

        let introspection = if configuration.server.introspection {
            Some(Arc::new(Introspection::new(&configuration).await))
        } else {
            None
        };

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner =
            BridgeQueryPlanner::new(self.schema.clone(), introspection, configuration)
                .await
                .map_err(ServiceBuildError::QueryPlannerError)?;
        let query_planner_service =
            CachingQueryPlanner::new(bridge_query_planner, plan_cache_limit).await;

        let plugins = Arc::new(self.plugins);

        let subgraph_creator = Arc::new(SubgraphCreator::new(
            self.subgraph_services,
            plugins.clone(),
        ));

        let apq = APQLayer::with_cache(DeduplicatingCache::new().await);

        Ok(RouterCreator {
            query_planner_service,
            subgraph_creator,
            schema: self.schema,
            plugins,
            apq,
        })
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct RouterCreator {
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    subgraph_creator: Arc<SubgraphCreator>,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
    apq: APQLayer,
}

impl NewService<http::Request<graphql::Request>> for RouterCreator {
    type Service = BoxService<
        http::Request<graphql::Request>,
        http::Response<graphql::ResponseStream>,
        BoxError,
    >;
    fn new_service(&self) -> Self::Service {
        self.make()
            .map_request(|http_request: http::Request<graphql::Request>| http_request.into())
            .map_response(|response| response.response)
            .boxed()
    }
}

impl SupergraphServiceFactory for RouterCreator {
    type SupergraphService = BoxService<
        http::Request<graphql::Request>,
        http::Response<graphql::ResponseStream>,
        BoxError,
    >;

    type Future =
        <<RouterCreator as NewService<http::Request<graphql::Request>>>::Service as Service<
            http::Request<graphql::Request>,
        >>::Future;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut mm = MultiMap::new();
        self.plugins
            .values()
            .for_each(|p| mm.extend(p.web_endpoints()));
        mm
    }
}

impl RouterCreator {
    pub(crate) fn make(
        &self,
    ) -> impl Service<
        SupergraphRequest,
        Response = SupergraphResponse,
        Error = BoxError,
        Future = BoxFuture<'static, Result<SupergraphResponse, BoxError>>,
    > + Send {
        ServiceBuilder::new()
            .layer(self.apq.clone())
            .layer(EnsureQueryPresence::default())
            .service(
                self.plugins.iter().rev().fold(
                    BoxService::new(
                        SupergraphService::builder()
                            .query_planner_service(self.query_planner_service.clone())
                            .execution_service_factory(ExecutionCreator {
                                schema: self.schema.clone(),
                                plugins: self.plugins.clone(),
                                subgraph_creator: self.subgraph_creator.clone(),
                            })
                            .schema(self.schema.clone())
                            .build(),
                    ),
                    |acc, (_, e)| e.supergraph_service(acc),
                ),
            )
    }

    /// Create a test service.
    #[cfg(test)]
    pub(crate) fn test_service(
        &self,
    ) -> tower::util::BoxCloneService<SupergraphRequest, SupergraphResponse, BoxError> {
        use tower::buffer::Buffer;

        Buffer::new(self.make(), 512).boxed_clone()
    }
}
