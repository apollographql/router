//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::time::Instant;

use futures::FutureExt;
use futures::TryFutureExt;
use futures::future::BoxFuture;
use futures::future::ready;
use futures::stream::StreamExt;
use futures::stream::once;
use http::StatusCode;
use indexmap::IndexMap;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_service::Service;
use tracing::Span;
use tracing::field;
use tracing_futures::Instrument;

use crate::Configuration;
use crate::Context;
use crate::Notify;
use crate::apollo_studio_interop::UsageReporting;
use crate::batching::BatchQuery;
use crate::configuration::Batching;
use crate::configuration::PersistedQueriesPrewarmQueryPlanCache;
use crate::context::OPERATION_NAME;
use crate::error::CacheResolverError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::graphql::Response;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::ServiceBuilderExt;
use crate::plugin::DynPlugin;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::connectors::query_plans::store_connectors;
use crate::plugins::connectors::query_plans::store_connectors_labels;
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
use crate::plugins::subscription::Subscription;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEventResponse;
use crate::plugins::telemetry::consts::QUERY_PLANNING_SPAN_NAME;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::InMemoryCachePlanner;
use crate::query_planner::QueryPlannerService;
use crate::query_planner::subscription::OPENED_SUBSCRIPTIONS;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::ExecutionServiceFactory;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerResponse;
use crate::services::SubgraphServiceFactory;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::connector::request_service::ConnectorRequestServiceFactory;
use crate::services::connector_service::ConnectorServiceFactory;
use crate::services::execution;
use crate::services::execution::QueryPlan;
use crate::services::fetch_service::FetchServiceFactory;
use crate::services::http::HttpClientServiceFactory;
use crate::services::layers::allow_only_http_post_mutations::AllowOnlyHttpPostMutationsLayer;
use crate::services::layers::content_negotiation;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::query_planner;
use crate::services::router::ClientRequestAccepts;
use crate::services::subgraph::BoxGqlStream;
use crate::services::subgraph_service::MakeSubgraphService;
use crate::services::supergraph;
use crate::spec::Schema;
use crate::spec::operation_limits::OperationLimits;

pub(crate) const FIRST_EVENT_CONTEXT_KEY: &str = "apollo::supergraph::first_event";
pub(crate) const SUBSCRIPTION_ERROR_EXTENSION_KEY: &str = "apollo::subscriptions::fatal_error";
const SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE: &str = "SUBSCRIPTION_CONFIG_RELOAD";
const SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE: &str = "SUBSCRIPTION_SCHEMA_RELOAD";
const SUBSCRIPTION_JWT_EXPIRED_EXTENSION_CODE: &str = "SUBSCRIPTION_JWT_EXPIRED";
const SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE: &str = "SUBSCRIPTION_EXECUTION_ERROR";

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecycle.
#[derive(Clone)]
pub(crate) struct SupergraphService {
    query_planner_service: CachingQueryPlanner<QueryPlannerService>,
    execution_service: execution::BoxCloneService,
    schema: Arc<Schema>,
    notify: Notify<String, graphql::Response>,
}

#[buildstructor::buildstructor]
impl SupergraphService {
    #[builder]
    pub(crate) fn new(
        query_planner_service: CachingQueryPlanner<QueryPlannerService>,
        execution_service: execution::BoxCloneService,
        schema: Arc<Schema>,
        notify: Notify<String, graphql::Response>,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service,
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
        if let Some(connectors) = &self.schema.connectors {
            store_connectors_labels(&req.context, connectors.labels_by_service_name.clone());
            store_connectors(&req.context, connectors.by_service_name.clone());
        }

        // Consume our cloned services and allow ownership to be transferred to the async block.
        let clone = self.query_planner_service.clone();

        let planning = std::mem::replace(&mut self.query_planner_service, clone);

        let schema = self.schema.clone();

        let context_cloned = req.context.clone();
        let fut = service_call(
            planning,
            self.execution_service.clone(),
            schema,
            req,
            self.notify.clone(),
        )
        .or_else(|error: BoxError| async move {
            let errors = vec![
                crate::error::Error::builder()
                    .message(error.to_string())
                    .extension_code("INTERNAL_SERVER_ERROR")
                    .build(),
            ];

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
    planning: CachingQueryPlanner<QueryPlannerService>,
    execution_service: execution::BoxCloneService,
    schema: Arc<Schema>,
    req: SupergraphRequest,
    notify: Notify<String, graphql::Response>,
) -> Result<SupergraphResponse, BoxError> {
    let context = req.context;
    let body = req.supergraph_request.body();
    let variables = body.variables.clone();

    let QueryPlannerResponse { content, errors } = match plan_query(
        planning,
        body.operation_name.clone(),
        context.clone(),
        // We cannot assume that the query is present as it may have been modified by coprocessors or plugins.
        // There is a deeper issue here in that query analysis is doing a bunch of stuff that it should not and
        // places the results in context. Therefore plugins that have modified the query won't actually take effect.
        // However, this can't be resolved before looking at the pipeline again.
        req.supergraph_request
            .body()
            .query
            .clone()
            .unwrap_or_default(),
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            let status = match &err {
                CacheResolverError::Backpressure(_) => StatusCode::SERVICE_UNAVAILABLE,
                CacheResolverError::RetrievalError(_) | CacheResolverError::BatchingError(_) => {
                    StatusCode::BAD_REQUEST
                }
            };
            match err.into_graphql_errors() {
                Ok(gql_errors) => {
                    return Ok(SupergraphResponse::infallible_builder()
                        .context(context)
                        .errors(gql_errors)
                        .status_code(status) // If it's a graphql error we return a status code 400
                        .build());
                }
                Err(err) => return Err(err.into()),
            }
        }
    };

    if !errors.is_empty() {
        return Ok(SupergraphResponse::infallible_builder()
            .context(context)
            .errors(errors)
            .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
            .build());
    }

    match content {
        Some(QueryPlannerContent::Response { response })
        | Some(QueryPlannerContent::CachedIntrospectionResponse { response }) => Ok(
            SupergraphResponse::new_from_graphql_response(*response, context),
        ),
        Some(QueryPlannerContent::IntrospectionDisabled) => {
            let mut response = SupergraphResponse::new_from_graphql_response(
                graphql::Response::builder()
                    .errors(vec![
                        crate::error::Error::builder()
                            .message(String::from("introspection has been disabled"))
                            .extension_code("INTROSPECTION_DISABLED")
                            .build(),
                    ])
                    .build(),
                context,
            );
            *response.response.status_mut() = StatusCode::BAD_REQUEST;
            Ok(response)
        }

        Some(QueryPlannerContent::Plan { plan }) => {
            let query_metrics = plan.query_metrics;
            context.extensions().with_lock(|lock| {
                let _ = lock.insert::<OperationLimits<u32>>(query_metrics);
            });

            let is_deferred = plan.is_deferred(&variables);
            let is_subscription = plan.is_subscription();

            if let Some(batching) = context
                .extensions()
                .with_lock(|lock| lock.get::<Batching>().cloned())
            {
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
                let batch_query_opt = context
                    .extensions()
                    .with_lock(|lock| lock.get::<BatchQuery>().cloned());
                if let Some(batch_query) = batch_query_opt {
                    let query_hashes = plan.query_hashes(batching, &variables)?;
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
                .with_lock(|lock| lock.get().cloned())
                .unwrap_or_default();
            let mut subscription_tx = None;
            if (is_deferred && !accepts_multipart_defer)
                || (is_subscription && !accepts_multipart_subscription)
            {
                let (error_message, error_code) = if is_deferred {
                    (
                        String::from(
                            "the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed;deferSpec=20220824'",
                        ),
                        "DEFER_BAD_HEADER",
                    )
                } else {
                    (
                        String::from(
                            "the router received a query with a subscription but the client does not accept multipart/mixed HTTP responses. To enable subscription support, add the HTTP header 'Accept: multipart/mixed;subscriptionSpec=1.0'",
                        ),
                        "SUBSCRIPTION_BAD_HEADER",
                    )
                };
                let mut response = SupergraphResponse::new_from_graphql_response(
                    graphql::Response::builder()
                        .errors(vec![
                            crate::error::Error::builder()
                                .message(error_message)
                                .extension_code(error_code)
                                .build(),
                        ])
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
                    let execution_service_cloned = execution_service.clone();
                    let cloned_supergraph_req =
                        clone_supergraph_request(&req.supergraph_request, context.clone());
                    // Spawn task for subscription
                    tokio::spawn(async move {
                        subscription_task(
                            execution_service_cloned,
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

                let execution_response = execution_service
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
                    .with_lock(|lock| lock.get::<SupergraphEventResponse>().cloned());
                let mut first_event = true;
                let mut inserted = false;
                let ctx = context.clone();
                let response_stream = response_stream.inspect(move |_| {
                    if first_event {
                        first_event = false;
                    } else if !inserted {
                        ctx.insert_json_value(
                            FIRST_EVENT_CONTEXT_KEY,
                            serde_json_bytes::Value::Bool(false),
                        );
                        inserted = true;
                    }
                });

                // make sure to resolve the first part of the stream - that way we know context
                // variables (`FIRST_EVENT_CONTEXT_KEY`, `CONTAINS_GRAPHQL_ERROR`) have been set
                let (first, remaining) = StreamExt::into_future(response_stream).await;
                let response_stream = once(ready(first.unwrap_or_default()))
                    .chain(remaining)
                    .boxed();

                match supergraph_response_event {
                    Some(supergraph_response_event) => {
                        let mut attrs = Vec::with_capacity(4);
                        attrs.push(KeyValue::new(
                            Key::from_static_str("http.response.headers"),
                            opentelemetry::Value::String(format!("{:?}", parts.headers).into()),
                        ));
                        attrs.push(KeyValue::new(
                            Key::from_static_str("http.response.status"),
                            opentelemetry::Value::String(format!("{}", parts.status).into()),
                        ));
                        attrs.push(KeyValue::new(
                            Key::from_static_str("http.response.version"),
                            opentelemetry::Value::String(format!("{:?}", parts.version).into()),
                        ));
                        let ctx = context.clone();
                        let response_stream = Box::pin(response_stream.inspect(move |resp| {
                            if !supergraph_response_event
                                .condition
                                .evaluate_event_response(resp, &ctx)
                            {
                                return;
                            }
                            attrs.push(KeyValue::new(
                                Key::from_static_str("http.response.body"),
                                opentelemetry::Value::String(
                                    serde_json::to_string(resp).unwrap_or_default().into(),
                                ),
                            ));
                            log_event(
                                supergraph_response_event.level,
                                "supergraph.response",
                                attrs.clone(),
                                "",
                            );
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
}

fn subscription_fatal_error(message: impl Into<String>, extension_code: &str) -> Response {
    Response::builder()
        .subscribed(false)
        .extension(SUBSCRIPTION_ERROR_EXTENSION_KEY, true)
        .error(
            graphql::Error::builder()
                .message(message)
                .extension_code(extension_code)
                .build(),
        )
        .build()
}

#[allow(clippy::too_many_arguments)]
async fn subscription_task(
    execution_service: execution::BoxCloneService,
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
    let mut receiver = sub_params.stream_rx;
    let sender = sub_params.client_sender;

    // Get the rest of the query_plan to execute for subscription events
    let query_plan = match &*query_plan.root {
        crate::query_planner::PlanNode::Subscription { rest, .. } => rest.clone().map(|r| {
            Arc::new(QueryPlan {
                usage_reporting: query_plan.usage_reporting.clone(),
                root: Arc::new(*r),
                formatted_query_plan: query_plan.formatted_query_plan.clone(),
                query: query_plan.query.clone(),
                query_metrics: query_plan.query_metrics,
                estimated_size: Default::default(),
            })
        }),
        _ => {
            let _ = sender
                .send(subscription_fatal_error(
                    "cannot execute the subscription event",
                    SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE,
                ))
                .await;
            return;
        }
    };

    let limit_is_set = subscription_config.max_opened_subscriptions.is_some();
    let mut subscription_handle = subscription_handle.clone();
    let operation_signature = context
        .extensions()
        .with_lock(|lock| {
            lock.get::<Arc<UsageReporting>>()
                .map(|usage_reporting| usage_reporting.get_stats_report_key().clone())
        })
        .unwrap_or_default();

    let operation_name = context
        .get::<_, String>(OPERATION_NAME)
        .ok()
        .flatten()
        .unwrap_or_default();

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

    let mut timeout = if supergraph_req
        .context
        .get_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .is_some()
    {
        let expires_in =
            crate::plugins::authentication::jwks::jwt_expires_in(&supergraph_req.context);
        tokio::time::sleep(expires_in).boxed()
    } else {
        futures::future::pending().boxed()
    };

    loop {
        tokio::select! {
            // We prefer to specify the order of checks within the select
            biased;
            _ = subscription_handle.closed_signal.recv() => {
                break;
            }
            _ = &mut timeout => {
                let _ = sender.send(subscription_fatal_error("subscription closed because the JWT has expired", SUBSCRIPTION_JWT_EXPIRED_EXTENSION_CODE)).await;
                break;
            },
            message = receiver.next() => {
                match message {
                    Some(mut val) => {
                        val.created_at = Some(Instant::now());
                        let res = dispatch_subscription_event(&supergraph_req, execution_service.clone(), query_plan.as_ref(), context.clone(), val, sender.clone())
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
            Some(_new_configuration) = configuration_updated_rx.next() => {
                let _ = sender
                    .send(subscription_fatal_error("subscription has been closed due to a configuration reload", SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE))
                    .await;
            }
            Some(_new_schema) = schema_updated_rx.next() => {
                let _ = sender
                    .send(subscription_fatal_error("subscription has been closed due to a schema reload", SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE))
                    .await;

                break;
            }
        }
    }
    drop(sender);
    tracing::trace!("Leaving the task for subscription");
    if limit_is_set {
        OPENED_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn dispatch_subscription_event(
    supergraph_req: &SupergraphRequest,
    execution_service: execution::BoxCloneService,
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

            let execution_response = execution_service.oneshot(execution_request).await;
            let next_response = match execution_response {
                Ok(mut execution_response) => execution_response.next_response().await,
                Err(err) => {
                    tracing::error!("cannot execute the subscription event: {err:?}");
                    let _ = sender
                        .send(subscription_fatal_error(
                            "cannot execute the subscription event",
                            SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE,
                        ))
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
    mut planning: CachingQueryPlanner<QueryPlannerService>,
    operation_name: Option<String>,
    context: Context,
    query_str: String,
) -> Result<QueryPlannerResponse, CacheResolverError> {
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
    http_service_factory: IndexMap<String, HttpClientServiceFactory>,
    configuration: Option<Arc<Configuration>>,
    planner: QueryPlannerService,
}

impl PluggableSupergraphServiceBuilder {
    pub(crate) fn new(planner: QueryPlannerService) -> Self {
        Self {
            plugins: Arc::new(Default::default()),
            subgraph_services: Default::default(),
            http_service_factory: Default::default(),
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

    pub(crate) fn with_http_service_factory(
        mut self,
        http_service_factory: IndexMap<String, HttpClientServiceFactory>,
    ) -> PluggableSupergraphServiceBuilder {
        self.http_service_factory = http_service_factory;
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
        let subgraph_schemas = self.planner.subgraph_schemas();

        let query_planner_service = CachingQueryPlanner::new(
            self.planner,
            schema.clone(),
            subgraph_schemas.clone(),
            &configuration,
            IndexMap::default(),
        )
        .await?;

        // Activate the telemetry plugin.
        // We must NOT fail to go live with the new router from this point as the telemetry plugin activate interacts with globals.
        for (_, plugin) in self.plugins.iter() {
            plugin.activate();
        }

        // We need a non-fallible hook so that once we know we are going live with a pipeline we do final initialization.
        // For now just shoe-horn something in, but if we ever reintroduce the query planner hook in plugins and activate then this can be made clean.
        query_planner_service.activate();

        let subscription_plugin_conf = self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == APOLLO_SUBSCRIPTION_PLUGIN)
            .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Subscription>())
            .map(|p| p.config.clone());

        let connector_sources = schema
            .connectors
            .as_ref()
            .map(|c| c.source_config_keys.clone())
            .unwrap_or_default();

        let fetch_service_factory = Arc::new(FetchServiceFactory::new(
            schema.clone(),
            subgraph_schemas.clone(),
            Arc::new(SubgraphServiceFactory::new(
                self.subgraph_services
                    .into_iter()
                    .map(|(name, service)| (name, service.into()))
                    .collect(),
                self.plugins.clone(),
            )),
            subscription_plugin_conf.clone(),
            Arc::new(ConnectorServiceFactory::new(
                schema.clone(),
                subgraph_schemas,
                subscription_plugin_conf,
                schema
                    .connectors
                    .as_ref()
                    .map(|c| c.by_service_name.clone())
                    .unwrap_or_default(),
                Arc::new(ConnectorRequestServiceFactory::new(
                    Arc::new(self.http_service_factory),
                    self.plugins.clone(),
                    connector_sources,
                )),
            )),
        ));

        let execution_service_factory = ExecutionServiceFactory {
            schema: schema.clone(),
            subgraph_schemas: query_planner_service.subgraph_schemas(),
            plugins: self.plugins.clone(),
            fetch_service_factory,
        };

        let execution_service: execution::BoxCloneService = ServiceBuilder::new()
            .buffered()
            .service(execution_service_factory.create())
            .boxed_clone();

        let supergraph_service = SupergraphService::builder()
            .query_planner_service(query_planner_service.clone())
            .execution_service(execution_service)
            .schema(schema.clone())
            .notify(configuration.notify.clone())
            .build();

        let supergraph_service =
            AllowOnlyHttpPostMutationsLayer::default().layer(supergraph_service);

        let sb = Buffer::new(
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
                .boxed(),
            DEFAULT_BUFFER_SIZE,
        );

        Ok(SupergraphCreator {
            query_planner_service,
            schema,
            plugins: self.plugins,
            sb,
        })
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct SupergraphCreator {
    query_planner_service: CachingQueryPlanner<QueryPlannerService>,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
    sb: Buffer<supergraph::Request, BoxFuture<'static, supergraph::ServiceResult>>,
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
    > + Send
    + use<> {
        // Note: We have to box our cloned service to erase the type of the Buffer.
        self.sb.clone().boxed()
    }

    pub(crate) fn previous_cache(&self) -> InMemoryCachePlanner {
        self.query_planner_service.previous_cache()
    }

    pub(crate) async fn warm_up_query_planner(
        &mut self,
        query_parser: &QueryAnalysisLayer,
        persisted_query_layer: &PersistedQueryLayer,
        previous_cache: Option<InMemoryCachePlanner>,
        count: Option<usize>,
        experimental_reuse_query_plans: bool,
        experimental_pql_prewarm: &PersistedQueriesPrewarmQueryPlanCache,
    ) {
        self.query_planner_service
            .warm_up(
                query_parser,
                persisted_query_layer,
                previous_cache,
                count,
                experimental_reuse_query_plans,
                experimental_pql_prewarm,
            )
            .await
    }
}
