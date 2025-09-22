//! The execution-side end of the subscriptions side-channel, which propagates messages from the
//! subgraph to the client.
//!
//! This end receives the messages from the subgraph, executes query plans to resolve federated
//! data in those messages, and sends the response back on a channel that is part of the eventual
//! response.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use futures::FutureExt;
use futures::stream::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;
use tracing::Span;
use tracing::field;
use tracing_futures::Instrument;

use crate::Context;
use crate::Notify;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::OPERATION_NAME;
use crate::graphql;
use crate::graphql::Response;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::subscription::SUBSCRIPTION_ERROR_EXTENSION_KEY;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use crate::query_planner::subscription::OPENED_SUBSCRIPTIONS;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::services::ExecutionRequest;
use crate::services::SupergraphRequest;
use crate::services::execution;
use crate::services::execution::QueryPlan;
use crate::services::subgraph::BoxGqlStream;

const SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE: &str = "SUBSCRIPTION_CONFIG_RELOAD";
const SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE: &str = "SUBSCRIPTION_SCHEMA_RELOAD";
const SUBSCRIPTION_JWT_EXPIRED_EXTENSION_CODE: &str = "SUBSCRIPTION_JWT_EXPIRED";
const SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE: &str = "SUBSCRIPTION_EXECUTION_ERROR";

pub(crate) struct SubscriptionTaskParams {
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
pub(crate) async fn subscription_task(
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
