//! NATS JetStream pull-consumer adapter.

use std::num::NonZeroU64;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::consumer::AckPolicy;
use async_nats::jetstream::consumer::DeliverPolicy;
use futures::StreamExt;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use super::EventError;
use super::ProviderEvent;
use super::ProviderEventStream;
use super::nats::NatsProvider;
use super::providers::ProviderSubscription;
use crate::configuration::events::EventSourceConfiguration;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct JetStreamSourceOptions {
    stream: String,
    domain: Option<String>,
    #[serde(with = "humantime_serde")]
    ack_wait: Duration,
    #[serde(with = "humantime_serde")]
    inactive_threshold: Duration,
    max_deliver: Option<NonZeroU64>,
}

impl Default for JetStreamSourceOptions {
    fn default() -> Self {
        Self {
            stream: String::new(),
            domain: None,
            ack_wait: Duration::from_secs(30),
            inactive_threshold: Duration::from_secs(60),
            max_deliver: None,
        }
    }
}

pub(super) async fn subscribe(
    provider: &NatsProvider,
    options: &JetStreamSourceOptions,
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
            "NATS JetStream source '{source_name}' must have at least one subject"
        )));
    }

    let client = provider.client(provider_name).await?;
    let context = match &options.domain {
        Some(domain) => jetstream::context::ContextBuilder::new()
            .domain(domain)
            .build(client),
        None => jetstream::new(client),
    };
    let stream = context.get_stream(&options.stream).await.map_err(|error| {
        EventError::new(format!(
            "could not open JetStream stream '{}' for source '{source_name}': {error}",
            options.stream
        ))
    })?;
    let consumer = stream
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: None,
            name: None,
            description: Some(format!("Apollo Router event source {source_name}")),
            deliver_policy: DeliverPolicy::New,
            ack_policy: AckPolicy::Explicit,
            ack_wait: options.ack_wait,
            max_deliver: options
                .max_deliver
                .map(|value| value.get().min(i64::MAX as u64) as i64)
                .unwrap_or_default(),
            filter_subjects: destinations,
            max_ack_pending: buffer_capacity.min(i64::MAX as usize) as i64,
            inactive_threshold: options.inactive_threshold,
            ..Default::default()
        })
        .await
        .map_err(|error| {
            EventError::new(format!(
                "could not create JetStream consumer for source '{source_name}': {error}"
            ))
        })?;
    let mut messages = consumer.messages().await.map_err(|error| {
        EventError::new(format!(
            "could not consume JetStream source '{source_name}': {error}"
        ))
    })?;

    let (sender, receiver) = tokio::sync::mpsc::channel(buffer_capacity);
    let source_name = source_name.to_string();
    tokio::spawn(async move {
        while let Some(result) = messages.next().await {
            let message = match result {
                Ok(message) => message,
                Err(error) => {
                    if sender
                        .send(Err(EventError::new(format!(
                            "JetStream source '{source_name}' receive error: {error}"
                        ))))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
            };
            let event = ProviderEvent {
                payload: message.payload.clone(),
            };
            if sender.send(Ok(event)).await.is_err() {
                break;
            }
            if let Err(error) = message.ack().await {
                tracing::warn!(source = %source_name, %error, "could not acknowledge JetStream event");
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

fn invalid_source(source_name: &str, error: impl std::fmt::Display) -> EventError {
    EventError::new(format!(
        "invalid NATS JetStream source '{source_name}' configuration: {error}"
    ))
}

pub(super) fn parse_source(
    source_name: &str,
    source: &EventSourceConfiguration,
) -> Result<JetStreamSourceOptions, EventError> {
    let options = serde_json::from_value::<JetStreamSourceOptions>(serde_json::Value::Object(
        source.provider_options.clone(),
    ))
    .map_err(|error| invalid_source(source_name, error))?;
    if options.stream.trim().is_empty() {
        return Err(EventError::new(format!(
            "NATS JetStream source '{source_name}' must configure a stream"
        )));
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_consumer_controls() {
        let options: JetStreamSourceOptions = serde_json::from_value(serde_json::json!({
            "stream": "PRODUCTS",
            "domain": "hub",
            "ack_wait": "45s",
            "inactive_threshold": "2m",
            "max_deliver": 5
        }))
        .expect("configuration is valid");
        assert_eq!(options.stream, "PRODUCTS");
        assert_eq!(options.ack_wait, Duration::from_secs(45));
        assert_eq!(options.max_deliver.map(NonZeroU64::get), Some(5));
    }

    #[test]
    fn rejects_durable_consumer_configuration() {
        let error = serde_json::from_value::<JetStreamSourceOptions>(serde_json::json!({
            "stream": "PRODUCTS",
            "durable_name": "shared"
        }))
        .expect_err("shared durable consumers violate every_router_instance");
        assert!(error.to_string().contains("unknown field"));
    }
}
