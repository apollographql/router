//! Implements the router phase of the request lifecycle.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Poll;
use std::time::Instant;

use futures::future::BoxFuture;
use futures::stream::StreamExt;
use futures::TryFutureExt;
use http::StatusCode;
use indexmap::IndexMap;
use router_bridge::planner::Planner;
use router_bridge::planner::UsageReporting;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::field;
use tracing::Span;
use tracing_futures::Instrument;

use crate::batching::BatchQuery;
use crate::configuration::Batching;
use crate::context::OPERATION_NAME;
use crate::error::CacheResolverError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::graphql::Response;
use crate::plugin::DynPlugin;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::events::SupergraphEventResponseLevel;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::query_planner::subscription::OPENED_SUBSCRIPTIONS;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::query_planner::BridgeQueryPlannerPool;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::InMemoryCachePlanner;
use crate::query_planner::QueryPlanResult;
use crate::router_factory::create_plugins;
use crate::router_factory::create_subgraph_services;
use crate::services::execution::QueryPlan;
use crate::services::layers::allow_only_http_post_mutations::AllowOnlyHttpPostMutationsLayer;
use crate::services::layers::content_negotiation;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::query_planner;
use crate::services::router::ClientRequestAccepts;
use crate::services::subgraph::BoxGqlStream;
use crate::services::subgraph_service::MakeSubgraphService;
use crate::services::subgraph_service::SubgraphServiceFactory;
use crate::services::supergraph;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::ExecutionServiceFactory;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;
use crate::Notify;

pub(crate) const QUERY_PLANNING_SPAN_NAME: &str = "query_planning";

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct SupergraphService {
    execution_service_factory: ExecutionServiceFactory,
    query_planner_service: CachingQueryPlanner<BridgeQueryPlannerPool>,
    schema: Arc<Schema>,
    notify: Notify<String, graphql::Response>,
}

#[buildstructor::buildstructor]
impl SupergraphService {
    #[builder]
    pub(crate) fn new(
        query_planner_service: CachingQueryPlanner<BridgeQueryPlannerPool>,
        execution_service_factory: ExecutionServiceFactory,
        schema: Arc<Schema>,
        notify: Notify<String, graphql::Response>,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service_factory,
            schema,
            notify,
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

        let schema = self.schema.clone();

        let context_cloned = req.context.clone();
        let fut = service_call(
            planning,
            self.execution_service_factory.clone(),
            schema,
            req,
            self.notify.clone(),
        )
        .or_else(|error: BoxError| async move {
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

            Ok(SupergraphResponse::infallible_builder()
                .errors(errors)
                .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                .context(context_cloned)
                .build())
        });

        Box::pin(fut)
    }
}

async fn service_call(
    planning: CachingQueryPlanner<BridgeQueryPlannerPool>,
    execution_service_factory: ExecutionServiceFactory,
    schema: Arc<Schema>,
    req: SupergraphRequest,
    notify: Notify<String, graphql::Response>,
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
                return Ok(SupergraphResponse::infallible_builder()
                    .context(context)
                    .errors(gql_errors)
                    .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
                    .build());
            }
            Err(err) => return Err(err.into()),
        },
    };

    if !errors.is_empty() {
        return Ok(SupergraphResponse::infallible_builder()
            .context(context)
            .errors(errors)
            .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
            .build());
    }

    match content {
        Some(QueryPlannerContent::Response { response }) => Ok(
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

            if let Some(batching) = {
                let lock = context.extensions().lock();
                let batching = lock.get::<Batching>();
                batching.cloned()
            } {
                if batching.enabled && (is_deferred || is_subscription) {
                    let message = if is_deferred {
                        "BATCHING_DEFER_UNSUPPORTED"
                    } else {
                        "BATCHING_SUBSCRIPTION_UNSUPPORTED"
                    };
                    let mut response = SupergraphResponse::new_from_graphql_response(
                            graphql::Response::builder()
                                .errors(vec![crate::error::Error::builder()
                                    .message(String::from(
                                        "Deferred responses and subscriptions aren't supported in batches",
                                    ))
                                    .extension_code(message)
                                    .build()])
                                .build(),
                            context.clone(),
                        );
                    *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                    return Ok(response);
                }
                // Now perform query batch analysis
                let batching = context.extensions().lock().get::<BatchQuery>().cloned();
                if let Some(batch_query) = batching {
                    let query_hashes = plan.query_hashes(operation_name.as_deref(), &variables)?;
                    batch_query
                        .set_query_hashes(query_hashes)
                        .await
                        .map_err(|e| CacheResolverError::BatchingError(e.to_string()))?;
                    tracing::debug!("batch registered: {}", batch_query);
                }
            }

            let ClientRequestAccepts {
                multipart_defer: accepts_multipart_defer,
                multipart_subscription: accepts_multipart_subscription,
                ..
            } = context
                .extensions()
                .lock()
                .get()
                .cloned()
                .unwrap_or_default();
            let mut subscription_tx = None;
            if (is_deferred && !accepts_multipart_defer)
                || (is_subscription && !accepts_multipart_subscription)
            {
                let (error_message, error_code) = if is_deferred {
                    (String::from("the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed;deferSpec=20220824'"), "DEFER_BAD_HEADER")
                } else {
                    (String::from("the router received a query with a subscription but the client does not accept multipart/mixed HTTP responses. To enable subscription support, add the HTTP header 'Accept: multipart/mixed;subscriptionSpec=1.0'"), "SUBSCRIPTION_BAD_HEADER")
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
                if is_subscription {
                    let ctx = context.clone();
                    let (subs_tx, subs_rx) = mpsc::channel(1);
                    let query_plan = plan.clone();
                    let execution_service_factory_cloned = execution_service_factory.clone();
                    let cloned_supergraph_req =
                        clone_supergraph_request(&req.supergraph_request, context.clone());
                    // Spawn task for subscription
                    tokio::spawn(async move {
                        subscription_task(
                            execution_service_factory_cloned,
                            ctx,
                            query_plan,
                            subs_rx,
                            notify,
                            cloned_supergraph_req,
                        )
                        .await;
                    });
                    subscription_tx = subs_tx.into();
                }

                let execution_response = execution_service_factory
                    .create()
                    .oneshot(
                        ExecutionRequest::internal_builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan.clone())
                            .context(context)
                            .and_subscription_tx(subscription_tx)
                            .build()
                            .await,
                    )
                    .await?;

                let ExecutionResponse { response, context } = execution_response;

                let (parts, response_stream) = response.into_parts();

                let supergraph_response_event = context
                    .extensions()
                    .lock()
                    .get::<SupergraphEventResponseLevel>()
                    .cloned();
                match supergraph_response_event {
                    Some(level) => {
                        let mut attrs = HashMap::with_capacity(4);
                        attrs.insert(
                            "http.response.headers".to_string(),
                            format!("{:?}", parts.headers),
                        );
                        attrs.insert(
                            "http.response.status".to_string(),
                            format!("{}", parts.status),
                        );
                        attrs.insert(
                            "http.response.version".to_string(),
                            format!("{:?}", parts.version),
                        );
                        let response_stream = Box::pin(response_stream.inspect(move |resp| {
                            attrs.insert(
                                "http.response.body".to_string(),
                                serde_json::to_string(resp).unwrap_or_default(),
                            );
                            log_event(level.0, "supergraph.response", attrs.clone(), "");
                        }));

                        Ok(SupergraphResponse {
                            context,
                            response: http::Response::from_parts(parts, response_stream.boxed()),
                        })
                    }
                    None => Ok(SupergraphResponse {
                        context,
                        response: http::Response::from_parts(parts, response_stream.boxed()),
                    }),
                }
            }
        }
        // This should never happen because if we have an empty query plan we should have error in errors vec
        None => Err(BoxError::from("cannot compute a query plan")),
    }
}

pub struct SubscriptionTaskParams {
    pub(crate) client_sender: tokio::sync::mpsc::Sender<Response>,
    pub(crate) subscription_handle: SubscriptionHandle,
    pub(crate) subscription_config: SubscriptionConfig,
    pub(crate) stream_rx: ReceiverStream<BoxGqlStream>,
    pub(crate) service_name: String,
}

async fn subscription_task(
    mut execution_service_factory: ExecutionServiceFactory,
    context: Context,
    query_plan: Arc<QueryPlan>,
    mut rx: mpsc::Receiver<SubscriptionTaskParams>,
    notify: Notify<String, graphql::Response>,
    supergraph_req: SupergraphRequest,
) {
    let sub_params = match rx.recv().await {
        Some(sub_params) => sub_params,
        None => {
            return;
        }
    };
    let subscription_config = sub_params.subscription_config;
    let subscription_handle = sub_params.subscription_handle;
    let service_name = sub_params.service_name;
    let mut receiver = sub_params.stream_rx;
    let sender = sub_params.client_sender;

    // Get the rest of the query_plan to execute for subscription events
    let query_plan = match &query_plan.root {
        crate::query_planner::PlanNode::Subscription { rest, .. } => rest.clone().map(|r| {
            Arc::new(QueryPlan {
                usage_reporting: query_plan.usage_reporting.clone(),
                root: *r,
                formatted_query_plan: query_plan.formatted_query_plan.clone(),
                query: query_plan.query.clone(),
            })
        }),
        _ => {
            let _ = sender
                .send(
                    graphql::Response::builder()
                        .error(
                            graphql::Error::builder()
                                .message("cannot execute the subscription event")
                                .extension_code("SUBSCRIPTION_EXECUTION_ERROR")
                                .build(),
                        )
                        .build(),
                )
                .await;
            return;
        }
    };

    let limit_is_set = subscription_config.max_opened_subscriptions.is_some();
    let mut subscription_handle = subscription_handle.clone();
    let operation_signature = context
        .extensions()
        .lock()
        .get::<Arc<UsageReporting>>()
        .map(|usage_reporting| usage_reporting.stats_report_key.clone())
        .unwrap_or_default();

    let operation_name = context
        .get::<_, String>(OPERATION_NAME)
        .ok()
        .flatten()
        .unwrap_or_default();
    let display_body = context.contains_key(LOGGING_DISPLAY_BODY);

    let mut receiver = match receiver.next().await {
        Some(receiver) => receiver,
        None => {
            tracing::trace!("receiver channel closed");
            return;
        }
    };

    if limit_is_set {
        OPENED_SUBSCRIPTIONS.fetch_add(1, Ordering::Relaxed);
    }

    let mut configuration_updated_rx = notify.subscribe_configuration();
    let mut schema_updated_rx = notify.subscribe_schema();

    let expires_in = crate::plugins::authentication::jwt_expires_in(&supergraph_req.context);

    let mut timeout = Box::pin(tokio::time::sleep(expires_in));

    loop {
        tokio::select! {
            // We prefer to specify the order of checks within the select
            biased;
            _ = subscription_handle.closed_signal.recv() => {
                break;
            }
            _ = &mut timeout => {
                let response = Response::builder()
                    .subscribed(false)
                    .error(
                        crate::error::Error::builder()
                            .message("subscription closed because the JWT has expired")
                            .extension_code("SUBSCRIPTION_JWT_EXPIRED")
                            .build(),
                    )
                    .build();
                let _ = sender.send(response).await;
                break;
            },
            message = receiver.next() => {
                match message {
                    Some(mut val) => {
                        if display_body {
                            tracing::info!(http.request.body = ?val, apollo.subgraph.name = %service_name, "Subscription event body from subgraph {service_name:?}");
                        }
                        val.created_at = Some(Instant::now());
                        let res = dispatch_event(&supergraph_req, &execution_service_factory, query_plan.as_ref(), context.clone(), val, sender.clone())
                            .instrument(tracing::info_span!(SUBSCRIPTION_EVENT_SPAN_NAME,
                                graphql.operation.name = %operation_name,
                                otel.kind = "INTERNAL",
                                apollo_private.operation_signature = %operation_signature,
                                apollo_private.duration_ns = field::Empty,)
                            ).await;
                        if let Err(err) = res {
                                tracing::error!("cannot send the subscription to the client: {err:?}");
                            break;
                        }
                    }
                    None => break,
                }
            }
            Some(new_configuration) = configuration_updated_rx.next() => {
                // If the configuration was dropped in the meantime, we ignore this update and will
                // pick up the next one.
                if let Some(conf) = new_configuration.upgrade() {
                    let plugins = match create_plugins(&conf, &execution_service_factory.schema, execution_service_factory.subgraph_schemas.clone(), None, None).await {
                        Ok(plugins) => Arc::new(plugins),
                        Err(err) => {
                            tracing::error!("cannot re-create plugins with the new configuration (closing existing subscription): {err:?}");
                            break;
                        },
                    };
                    let subgraph_services = match create_subgraph_services(&plugins, &execution_service_factory.schema, &conf).await {
                        Ok(subgraph_services) => subgraph_services,
                        Err(err) => {
                            tracing::error!("cannot re-create subgraph service with the new configuration (closing existing subscription): {err:?}");
                            break;
                        },
                    };

                    execution_service_factory = ExecutionServiceFactory {
                        schema: execution_service_factory.schema.clone(),
                        subgraph_schemas: execution_service_factory.subgraph_schemas.clone(),
                        plugins: plugins.clone(),
                        subgraph_service_factory: Arc::new(SubgraphServiceFactory::new(subgraph_services.into_iter().map(|(k, v)| (k, Arc::new(v) as Arc<dyn MakeSubgraphService>)).collect(), plugins.clone())),

                    };
                }
            }
            Some(new_schema) = schema_updated_rx.next() => {
                if new_schema.raw_sdl != execution_service_factory.schema.raw_sdl {
                    let _ = sender
                        .send(
                            Response::builder()
                                .subscribed(false)
                                .error(graphql::Error::builder().message("subscription has been closed due to a schema reload").extension_code("SUBSCRIPTION_SCHEMA_RELOAD").build())
                                .build(),
                        )
                        .await;

                    break;
                }
            }
        }
    }
    drop(sender);
    tracing::trace!("Leaving the task for subscription");
    if limit_is_set {
        OPENED_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn dispatch_event(
    supergraph_req: &SupergraphRequest,
    execution_service_factory: &ExecutionServiceFactory,
    query_plan: Option<&Arc<QueryPlan>>,
    context: Context,
    mut val: graphql::Response,
    sender: mpsc::Sender<Response>,
) -> Result<(), SendError<Response>> {
    let start = Instant::now();
    let span = Span::current();
    let res = match query_plan {
        Some(query_plan) => {
            let cloned_supergraph_req = clone_supergraph_request(
                &supergraph_req.supergraph_request,
                supergraph_req.context.clone(),
            );
            let execution_request = ExecutionRequest::internal_builder()
                .supergraph_request(cloned_supergraph_req.supergraph_request)
                .query_plan(query_plan.clone())
                .context(context)
                .source_stream_value(val.data.take().unwrap_or_default())
                .build()
                .await;

            let execution_service = execution_service_factory.create();
            let execution_response = execution_service.oneshot(execution_request).await;
            let next_response = match execution_response {
                Ok(mut execution_response) => execution_response.next_response().await,
                Err(err) => {
                    tracing::error!("cannot execute the subscription event: {err:?}");
                    let _ = sender
                        .send(
                            graphql::Response::builder()
                                .error(
                                    graphql::Error::builder()
                                        .message("cannot execute the subscription event")
                                        .extension_code("SUBSCRIPTION_EXECUTION_ERROR")
                                        .build(),
                                )
                                .build(),
                        )
                        .await;
                    return Ok(());
                }
            };

            if let Some(mut next_response) = next_response {
                next_response.created_at = val.created_at;
                next_response.subscribed = val.subscribed;
                val.errors.append(&mut next_response.errors);
                next_response.errors = val.errors;

                sender.send(next_response).await
            } else {
                Ok(())
            }
        }
        None => sender.send(val).await,
    };
    span.record(
        APOLLO_PRIVATE_DURATION_NS,
        start.elapsed().as_nanos() as i64,
    );

    res
}

async fn plan_query(
    mut planning: CachingQueryPlanner<BridgeQueryPlannerPool>,
    operation_name: Option<String>,
    context: Context,
    schema: Arc<Schema>,
    query_str: String,
) -> Result<QueryPlannerResponse, CacheResolverError> {
    // FIXME: we have about 80 tests creating a supergraph service and crafting a supergraph request for it
    // none of those tests create an executable document to put it in the context, and the document cannot be created
    // from inside the supergraph request fake builder, because it needs a schema matching the query.
    // So while we are updating the tests to create a document manually, this here will make sure current
    // tests will pass.
    // During a regular request, `ParsedDocument` is already populated during query analysis.
    // Some tests do populate the document, so we only do it if it's not already there.
    if !{
        let lock = context.extensions().lock();
        lock.contains_key::<crate::services::layers::query_analysis::ParsedDocument>()
    } {
        let doc = crate::spec::Query::parse_document(
            &query_str,
            operation_name.as_deref(),
            &schema,
            &Configuration::default(),
        )
        .map_err(crate::error::QueryPlannerError::from)?;
        context
            .extensions()
            .lock()
            .insert::<crate::services::layers::query_analysis::ParsedDocument>(doc);
    }

    let qpr = planning
        .call(
            query_planner::CachingRequest::builder()
                .query(query_str)
                .and_operation_name(operation_name)
                .context(context.clone())
                .build(),
        )
        .instrument(tracing::info_span!(
            QUERY_PLANNING_SPAN_NAME,
            "otel.kind" = "INTERNAL"
        ))
        .await?;

    Ok(qpr)
}

fn clone_supergraph_request(
    req: &http::Request<graphql::Request>,
    context: Context,
) -> SupergraphRequest {
    let mut cloned_supergraph_req = SupergraphRequest::builder()
        .extensions(req.body().extensions.clone())
        .and_query(req.body().query.clone())
        .context(context)
        .method(req.method().clone())
        .and_operation_name(req.body().operation_name.clone())
        .uri(req.uri().clone())
        .variables(req.body().variables.clone());

    for (header_name, header_value) in req.headers().clone() {
        if let Some(header_name) = header_name {
            cloned_supergraph_req = cloned_supergraph_req.header(header_name, header_value);
        }
    }

    cloned_supergraph_req
        .build()
        .expect("cloning an existing supergraph response should not fail")
}

/// Builder which generates a plugin pipeline.
///
/// This is at the heart of the delegation of responsibility model for the router. A schema,
/// collection of plugins, collection of subgraph services are assembled to generate a
/// [`tower::util::BoxCloneService`] capable of processing a router request
/// through the entire stack to return a response.
pub(crate) struct PluggableSupergraphServiceBuilder {
    plugins: Arc<Plugins>,
    subgraph_services: Vec<(String, Box<dyn MakeSubgraphService>)>,
    configuration: Option<Arc<Configuration>>,
    planner: BridgeQueryPlannerPool,
}

impl PluggableSupergraphServiceBuilder {
    pub(crate) fn new(planner: BridgeQueryPlannerPool) -> Self {
        Self {
            plugins: Arc::new(Default::default()),
            subgraph_services: Default::default(),
            configuration: None,
            planner,
        }
    }

    pub(crate) fn with_plugins(
        mut self,
        plugins: Arc<Plugins>,
    ) -> PluggableSupergraphServiceBuilder {
        self.plugins = plugins;
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
            .push((name.to_string(), Box::new(service_maker)));
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
        .await?;

        // Activate the telemetry plugin.
        // We must NOT fail to go live with the new router from this point as the telemetry plugin activate interacts with globals.
        for (_, plugin) in self.plugins.iter() {
            if let Some(telemetry) = plugin.as_any().downcast_ref::<Telemetry>() {
                telemetry.activate();
            }
        }

        /*for (_, service) in self.subgraph_services.iter_mut() {
            if let Some(subgraph) =
                (service as &mut dyn std::any::Any).downcast_mut::<SubgraphService>()
            {
                subgraph.client_factory.plugins = plugins.clone();
            }
        }*/

        let subgraph_service_factory = Arc::new(SubgraphServiceFactory::new(
            self.subgraph_services
                .into_iter()
                .map(|(name, service)| (name, service.into()))
                .collect(),
            self.plugins.clone(),
        ));

        Ok(SupergraphCreator {
            query_planner_service,
            subgraph_service_factory,
            schema,
            plugins: self.plugins,
            config: configuration,
        })
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct SupergraphCreator {
    query_planner_service: CachingQueryPlanner<BridgeQueryPlannerPool>,
    subgraph_service_factory: Arc<SubgraphServiceFactory>,
    schema: Arc<Schema>,
    config: Arc<Configuration>,
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

pub(crate) trait HasConfig {
    fn config(&self) -> Arc<Configuration>;
}

impl HasConfig for SupergraphCreator {
    fn config(&self) -> Arc<Configuration> {
        Arc::clone(&self.config)
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
                subgraph_schemas: self.query_planner_service.subgraph_schemas(),
                plugins: self.plugins.clone(),
                subgraph_service_factory: self.subgraph_service_factory.clone(),
            })
            .schema(self.schema.clone())
            .notify(self.config.notify.clone())
            .build();

        let shaping = self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
            .and_then(|plugin| plugin.1.as_any().downcast_ref::<TrafficShaping>())
            .expect("traffic shaping should always be part of the plugin list");

        let supergraph_service = AllowOnlyHttpPostMutationsLayer::default()
            .layer(shaping.supergraph_service_internal(supergraph_service));

        ServiceBuilder::new()
            .layer(content_negotiation::SupergraphLayer::default())
            .service(
                self.plugins
                    .iter()
                    .rev()
                    .fold(supergraph_service.boxed(), |acc, (_, e)| {
                        e.supergraph_service(acc)
                    }),
            )
    }

    pub(crate) fn previous_cache(&self) -> InMemoryCachePlanner {
        self.query_planner_service.previous_cache()
    }

    pub(crate) fn planners(&self) -> Vec<Arc<Planner<QueryPlanResult>>> {
        self.query_planner_service.planners()
    }

    pub(crate) async fn warm_up_query_planner(
        &mut self,
        query_parser: &QueryAnalysisLayer,
        persisted_query_layer: &PersistedQueryLayer,
        previous_cache: InMemoryCachePlanner,
        count: Option<usize>,
        experimental_reuse_query_plans: bool,
    ) {
        self.query_planner_service
            .warm_up(
                query_parser,
                persisted_query_layer,
                previous_cache,
                count,
                experimental_reuse_query_plans,
            )
            .await
    }
}
