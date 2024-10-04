use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use tokio::sync::broadcast;

use super::rewrites;
use super::OperationKind;
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
    pub(crate) service_name: Arc<str>,

    /// The variables that are used for the subgraph subscription.
    pub(crate) variable_usages: Vec<Arc<str>>,

    /// The GraphQL subquery that is used for the subscription.
    pub(crate) operation: super::fetch::SubgraphOperation,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<Arc<str>>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this subscription.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a subscription (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,
}
