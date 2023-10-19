use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use futures::channel::mpsc;
use futures::future;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio::sync::broadcast;
use tower::ServiceExt;
use tracing_futures::Instrument;

use super::execution::ExecutionParameters;
use super::fetch::Variables;
use super::rewrites;
use super::OperationKind;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::http_ext;
use crate::json_ext::Path;
use crate::notification::HandleStream;
use crate::services::SubgraphRequest;
use crate::services::SubscriptionTaskParams;

pub(crate) const SUBSCRIPTION_EVENT_SPAN_NAME: &str = "subscription_event";
pub(crate) static OPENED_SUBSCRIPTIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct SubscriptionHandle {
    pub(crate) closed_signal: broadcast::Receiver<()>,
    pub(crate) subscription_conf_tx: Option<tokio::sync::mpsc::Sender<SubscriptionTaskParams>>,
}

impl Clone for SubscriptionHandle {
    fn clone(&self) -> Self {
        Self {
            closed_signal: self.closed_signal.resubscribe(),
            subscription_conf_tx: self.subscription_conf_tx.clone(),
        }
    }
}

impl SubscriptionHandle {
    pub(crate) fn new(
        closed_signal: broadcast::Receiver<()>,
        subscription_conf_tx: Option<tokio::sync::mpsc::Sender<SubscriptionTaskParams>>,
    ) -> Self {
        Self {
            closed_signal,
            subscription_conf_tx,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscriptionNode {
    /// The name of the service or subgraph that the subscription is querying.
    pub(crate) service_name: String,

    /// The variables that are used for the subgraph subscription.
    pub(crate) variable_usages: Vec<String>,

    /// The GraphQL subquery that is used for the subscription.
    pub(crate) operation: String,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<String>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this subscription.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a subscription (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,
}

impl SubscriptionNode {
    pub(crate) fn execute_recursively<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        current_dir: &'a Path,
        parent_value: &'a Value,
        sender: futures::channel::mpsc::Sender<Response>,
    ) -> future::BoxFuture<Vec<Error>> {
        if parameters.subscription_handle.is_none() {
            tracing::error!("No subscription handle provided for a subscription");
            return Box::pin(async {
                vec![Error::builder()
                    .message("no subscription handle provided for a subscription")
                    .extension_code("NO_SUBSCRIPTION_HANDLE")
                    .build()]
            });
        };
        if let Some(max_opened_subscriptions) = parameters
            .subscription_config
            .as_ref()
            .and_then(|s| s.max_opened_subscriptions)
        {
            if OPENED_SUBSCRIPTIONS.load(Ordering::Relaxed) >= max_opened_subscriptions {
                return Box::pin(async {
                    vec![Error::builder()
                        .message("can't open new subscription, limit reached")
                        .extension_code("SUBSCRIPTION_MAX_LIMIT")
                        .build()]
                });
            }
        }
        let subscription_handle = parameters
            .subscription_handle
            .as_ref()
            .expect("checked above; qed");
        let mode = match parameters.subscription_config.as_ref() {
            Some(config) => config
                .mode
                .get_subgraph_config(&self.service_name)
                .map(|mode| (config.clone(), mode)),
            None => {
                return Box::pin(async {
                    vec![Error::builder()
                        .message("subscription support is not enabled")
                        .extension_code("SUBSCRIPTION_DISABLED")
                        .build()]
                });
            }
        };

        Box::pin(async move {
            let mut subscription_handle = subscription_handle.clone();

            match mode {
                Some((subscription_config, _mode)) => {
                    let (tx_handle, rx_handle) =
                        mpsc::channel::<HandleStream<String, graphql::Response>>(1);

                    let subscription_conf_tx = match subscription_handle.subscription_conf_tx.take()
                    {
                        Some(sc) => sc,
                        None => {
                            return vec![Error::builder()
                                .message("no subscription conf sender provided for a subscription")
                                .extension_code("NO_SUBSCRIPTION_CONF_TX")
                                .build()];
                        }
                    };

                    let subs_params = SubscriptionTaskParams {
                        client_sender: sender,
                        subscription_handle,
                        subscription_config,
                        stream_rx: rx_handle,
                        service_name: self.service_name.clone(),
                    };

                    if let Err(err) = subscription_conf_tx.send(subs_params).await {
                        return vec![Error::builder()
                            .message(format!("cannot send the subscription data: {err:?}"))
                            .extension_code("SUBSCRIPTION_DATA_SEND_ERROR")
                            .build()];
                    }

                    match self
                        .subgraph_call(parameters, current_dir, parent_value, tx_handle)
                        .await
                    {
                        Ok(e) => e,
                        Err(err) => {
                            failfast_error!("subgraph call fetch error: {}", err);
                            vec![err.to_graphql_error(Some(current_dir.to_owned()))]
                        }
                    }
                }
                None => {
                    vec![Error::builder()
                        .message(format!(
                            "subscription mode is not configured for subgraph {:?}",
                            self.service_name
                        ))
                        .extension_code("INVALID_SUBSCRIPTION_MODE")
                        .build()]
                }
            }
        })
    }

    pub(crate) async fn subgraph_call<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        current_dir: &'a Path,
        data: &Value,
        tx_gql: mpsc::Sender<HandleStream<String, graphql::Response>>,
    ) -> Result<Vec<Error>, FetchError> {
        let SubscriptionNode {
            operation,
            operation_name,
            service_name,
            ..
        } = self;

        let Variables { variables, .. } = match Variables::new(
            &[],
            self.variable_usages.as_ref(),
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request,
            parameters.schema,
            &self.input_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return Ok(Vec::new());
            }
        };

        let subgraph_request = SubgraphRequest::builder()
            .supergraph_request(parameters.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(
                        parameters
                            .schema
                            .subgraph_url(service_name)
                            .unwrap_or_else(|| {
                                panic!(
                                    "schema uri for subgraph '{service_name}' should already have been checked"
                                )
                            })
                            .clone(),
                    )
                    .body(
                        Request::builder()
                            .query(operation)
                            .and_operation_name(operation_name.clone())
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .operation_kind(OperationKind::Subscription)
            .context(parameters.context.clone())
            .subscription_stream(tx_gql)
            .and_connection_closed_signal(parameters.subscription_handle.as_ref().map(|s| s.closed_signal.resubscribe()))
            .build();

        let service = parameters
            .service_factory
            .create(service_name)
            .expect("we already checked that the service exists during planning; qed");

        let (_parts, response) = service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subscription_call"))
            .await
            // TODO this is a problem since it restores details about failed service
            // when errors have been redacted in the include_subgraph_errors module.
            // Unfortunately, not easy to fix here, because at this point we don't
            // know if we should be redacting errors for this subgraph...
            .map_err(|e| FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                reason: e.to_string(),
                status_code: None,
            })?
            .response
            .into_parts();

        Ok(response.errors)
    }
}
