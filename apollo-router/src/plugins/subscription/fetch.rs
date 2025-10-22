//! Contains the core of the fetch service implementation for fetching subscription query plan
//! nodes.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures::future::BoxFuture;
use serde_json_bytes::Value;
use tokio::sync::mpsc;
use tower::BoxError;
use tower::ServiceExt;
use tracing::Instrument;
use tracing::instrument::Instrumented;

use crate::error::Error;
use crate::http_ext;
use crate::plugins::subscription::SubscriptionTaskParams;
use crate::query_planner::OperationKind;
use crate::query_planner::SUBSCRIBE_SPAN_NAME;
use crate::query_planner::subscription::OPENED_SUBSCRIPTIONS;
use crate::query_planner::subscription::SubscriptionNode;
use crate::services::FetchResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphServiceFactory;
use crate::services::fetch::ErrorMapping;
use crate::services::fetch::SubscriptionRequest;
use crate::services::subgraph::BoxGqlStream;
use crate::spec::Schema;

/// Execute the fetches required to fulfill a subscription query plan node.
///
/// Calls into the relevant subgraph service to run the actual requests. This means the
/// [SubgraphServiceFactory] must produce services that have the [SubscriptionSubgraphLayer][]
/// applied!
///
/// [SubscriptionSubgraphLayer]: super::subgraph::SubscriptionSubgraphLayer
pub(crate) fn fetch_service_handle_subscription(
    schema: Arc<Schema>,
    subgraph_service_factory: Arc<SubgraphServiceFactory>,
    request: SubscriptionRequest,
) -> Instrumented<BoxFuture<'static, Result<FetchResponse, BoxError>>> {
    let SubscriptionRequest {
        ref context,
        subscription_node: SubscriptionNode {
            ref service_name, ..
        },
        ..
    } = request;

    let service_name = service_name.clone();
    let fetch_time_offset = context.created_at.elapsed().as_nanos() as i64;

    // Subscriptions are not supported for connectors, so they always go to the subgraph service
    subscription_with_subgraph_service(schema, subgraph_service_factory, request).instrument(
        tracing::info_span!(
            SUBSCRIBE_SPAN_NAME,
            "otel.kind" = "INTERNAL",
            "apollo.subgraph.name" = service_name.as_ref(),
            "apollo_private.sent_time_offset" = fetch_time_offset
        ),
    )
}

fn subscription_with_subgraph_service(
    schema: Arc<Schema>,
    subgraph_service_factory: Arc<SubgraphServiceFactory>,
    request: SubscriptionRequest,
) -> BoxFuture<'static, Result<crate::services::fetch::Response, BoxError>> {
    let SubscriptionRequest {
        context,
        subscription_node,
        current_dir,
        sender,
        variables,
        supergraph_request,
        subscription_handle,
        subscription_config,
        ..
    } = request;
    let SubscriptionNode {
        ref service_name,
        ref operation,
        ref operation_name,
        ..
    } = subscription_node;

    let service_name = service_name.clone();

    if let Some(max_opened_subscriptions) = subscription_config
        .as_ref()
        .and_then(|s| s.max_opened_subscriptions)
        && OPENED_SUBSCRIPTIONS.load(Ordering::Relaxed) >= max_opened_subscriptions
    {
        return Box::pin(async {
            Ok((
                Value::default(),
                vec![
                    Error::builder()
                        .message("can't open new subscription, limit reached")
                        .extension_code("SUBSCRIPTION_MAX_LIMIT")
                        .build(),
                ],
            ))
        });
    }
    let mode = match subscription_config.as_ref() {
        Some(config) => config
            .mode
            .get_subgraph_config(&service_name)
            .map(|mode| (config.clone(), mode)),
        None => {
            return Box::pin(async {
                Ok((
                    Value::default(),
                    vec![
                        Error::builder()
                            .message("subscription support is not enabled")
                            .extension_code("SUBSCRIPTION_DISABLED")
                            .build(),
                    ],
                ))
            });
        }
    };

    let service = subgraph_service_factory
        .create(&service_name)
        .expect("we already checked that the service exists during planning; qed");

    let uri = schema
        .subgraph_url(service_name.as_ref())
        .unwrap_or_else(|| {
            panic!("schema uri for subgraph '{service_name}' should already have been checked")
        })
        .clone();

    let (tx_handle, rx_handle) = mpsc::channel::<BoxGqlStream>(1);

    let subscription_handle = subscription_handle
        .as_ref()
        .expect("checked in PlanNode; qed");

    let subgraph_request = SubgraphRequest::builder()
        .supergraph_request(supergraph_request.clone())
        .subgraph_request(
            http_ext::Request::builder()
                .method(http::Method::POST)
                .uri(uri)
                .body(
                    crate::graphql::Request::builder()
                        .query(operation.as_serialized())
                        .and_operation_name(operation_name.as_ref().map(|n| n.to_string()))
                        .variables(variables.variables.clone())
                        .build(),
                )
                .build()
                .expect("it won't fail because the url is correct and already checked; qed"),
        )
        .operation_kind(OperationKind::Subscription)
        .context(context)
        .subgraph_name(service_name.to_string())
        .subscription_stream(tx_handle)
        .and_connection_closed_signal(Some(subscription_handle.closed_signal.resubscribe()))
        .build();

    let mut subscription_handle = subscription_handle.clone();
    Box::pin(async move {
        let response = match mode {
            Some((subscription_config, _mode)) => {
                let subscription_params = SubscriptionTaskParams {
                    client_sender: sender,
                    subscription_handle: subscription_handle.clone(),
                    subscription_config: subscription_config.clone(),
                    stream_rx: rx_handle.into(),
                };

                let subscription_conf_tx =
                    match subscription_handle.subscription_conf_tx.take() {
                        Some(sc) => sc,
                        None => {
                            return Ok((
                                Value::default(),
                                vec![Error::builder()
                            .message("no subscription conf sender provided for a subscription")
                            .extension_code("NO_SUBSCRIPTION_CONF_TX")
                            .build()],
                            ));
                        }
                    };

                if let Err(err) = subscription_conf_tx.send(subscription_params).await {
                    return Ok((
                        Value::default(),
                        vec![
                            Error::builder()
                                .message(format!("cannot send the subscription data: {err:?}"))
                                .extension_code("SUBSCRIPTION_DATA_SEND_ERROR")
                                .build(),
                        ],
                    ));
                }

                match service
                    .oneshot(subgraph_request)
                    .instrument(tracing::trace_span!("subscription_call"))
                    .await
                    .map_to_graphql_error(service_name.to_string(), &current_dir)
                {
                    Err(e) => {
                        failfast_error!("subgraph call fetch error: {}", e);
                        vec![e]
                    }
                    Ok(response) => response.response.into_parts().1.errors,
                }
            }
            None => {
                vec![
                    Error::builder()
                        .message(format!(
                            "subscription mode is not configured for subgraph {service_name:?}"
                        ))
                        .extension_code("INVALID_SUBSCRIPTION_MODE")
                        .build(),
                ]
            }
        };
        Ok((Value::default(), response))
    })
}
