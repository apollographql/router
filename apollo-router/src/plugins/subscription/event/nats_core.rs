//! NATS Core adapter. Every Router process uses ordinary subscriptions so each
//! process receives every matching message; queue groups are intentionally not
//! used because they would violate `every_router_instance` distribution.

use futures::StreamExt;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use super::EventError;
use super::ProviderEvent;
use super::ProviderEventStream;
use super::nats::NatsProvider;
use super::providers::ProviderSubscription;
use crate::configuration::events::EventSourceConfiguration;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct NatsCoreSourceOptions {}

pub(super) async fn subscribe(
    provider: &NatsProvider,
    _options: &NatsCoreSourceOptions,
    subscription: ProviderSubscription<'_>,
) -> Result<ProviderEventStream, EventError> {
    let ProviderSubscription {
        provider_name,
        source_name,
        destinations,
        buffer_capacity,
        ..
    } = subscription;
    if destinations.is_empty() {
        return Err(EventError::new(format!(
            "NATS Core source '{source_name}' must have at least one subject"
        )));
    }

    let client = provider.client(provider_name).await?;
    let mut subscriptions = Vec::with_capacity(destinations.len());
    for subject in destinations {
        let subscription = client.subscribe(subject).await.map_err(|error| {
            EventError::new(format!(
                "could not subscribe NATS Core source '{source_name}': {error}"
            ))
        })?;
        subscriptions.push(subscription);
    }

    let mut messages = futures::stream::select_all(subscriptions);
    let (sender, receiver) = tokio::sync::mpsc::channel(buffer_capacity);
    tokio::spawn(async move {
        while let Some(message) = messages.next().await {
            let event = ProviderEvent {
                payload: message.payload,
            };
            if sender.send(Ok(event)).await.is_err() {
                break;
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

pub(super) fn parse_source(
    source_name: &str,
    source: &EventSourceConfiguration,
) -> Result<NatsCoreSourceOptions, EventError> {
    serde_json::from_value::<NatsCoreSourceOptions>(serde_json::Value::Object(
        source.provider_options.clone(),
    ))
    .map_err(|error| EventError::new(format!("invalid NATS Core source '{source_name}': {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_queue_group_source_option() {
        let error = serde_json::from_value::<NatsCoreSourceOptions>(serde_json::json!({
            "queue_group": "shared"
        }))
        .expect_err("queue groups violate every_router_instance");
        assert!(error.to_string().contains("unknown field"));
    }
}
