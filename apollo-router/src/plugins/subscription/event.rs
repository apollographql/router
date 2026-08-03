//! Provider-neutral event source integration for federated subscriptions.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::executable;
use futures::Stream;
use futures::StreamExt;
use tokio::sync::broadcast;

use crate::configuration::events::EventsConfiguration;
use crate::graphql;
use crate::query_planner::subscription::SubscriptionNode;
use crate::spec::Schema;

mod catalog;
mod decoder;
mod fanout;
mod kafka;
mod nats;
mod nats_core;
mod nats_jetstream;
mod providers;
mod redis_pubsub;
mod service;
mod template;

use catalog::EventCatalog;
use catalog::EventField;
use decoder::decode_graphql_entity;
use fanout::EventTrigger;
use fanout::TriggerAccess;
use fanout::TriggerRegistry;
use fanout::forward_shared_events;
use providers::ConfiguredEvents;
use providers::ConfiguredProvider;
use providers::ConfiguredSource;
use providers::ProviderSubscription;
pub(crate) use service::EventSubscriptionLayer;
pub(crate) use service::EventSubscriptionService;
use template::render_destinations;

/// A message before format decoding.
#[derive(Clone, Debug)]
pub(crate) struct ProviderEvent {
    pub(crate) payload: bytes::Bytes,
}

#[derive(Clone, Debug)]
pub(crate) struct EventError(String);

impl EventError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for EventError {}

pub(crate) type ProviderEventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderEvent, EventError>> + Send + Sync + 'static>>;

pub(crate) type EventStream =
    Pin<Box<dyn Stream<Item = Result<graphql::Response, EventError>> + Send + Sync + 'static>>;

/// Owns validated schema metadata and configured provider adapters for one router pipeline.
pub(crate) struct EventRuntime {
    events: ConfiguredEvents,
    catalog: EventCatalog,
    triggers: TriggerRegistry,
    instance_id: uuid::Uuid,
}

impl EventRuntime {
    pub(crate) fn try_new(
        schema: Arc<Schema>,
        configuration: EventsConfiguration,
    ) -> Result<Self, EventError> {
        let catalog = EventCatalog::from_schema(&schema)?;
        let events = ConfiguredEvents::parse(configuration)?;
        for ((service, field), event) in &catalog.fields {
            if !events.sources.contains_key(&event.source) {
                return Err(EventError::new(format!(
                    "event subscription field '{service}.{field}' references unconfigured source '{}'",
                    event.source
                )));
            }
            if event.destinations.is_empty() {
                return Err(EventError::new(format!(
                    "event subscription field '{service}.{field}' has no destinations"
                )));
            }
        }
        Ok(Self {
            events,
            catalog,
            triggers: TriggerRegistry::default(),
            instance_id: uuid::Uuid::new_v4(),
        })
    }

    fn event_field<'a>(
        &'a self,
        node: &'a SubscriptionNode,
    ) -> Option<(
        &'a EventField,
        String,
        &'a apollo_compiler::Node<executable::Field>,
    )> {
        let document = node.operation.as_parsed().ok()?;
        let operation = document
            .operations
            .get(node.operation_name.as_deref())
            .ok()?;
        let field = operation
            .selection_set
            .selections
            .iter()
            .find_map(|selection| {
                if let executable::Selection::Field(field) = selection {
                    Some(field)
                } else {
                    None
                }
            })?;
        let event = self
            .catalog
            .fields
            .get(&(node.service_name.to_string(), field.name.to_string()))?;
        let response_name = field
            .alias
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| field.name.to_string());
        Some((event, response_name, field))
    }

    fn is_event_subscription(&self, node: &SubscriptionNode) -> bool {
        self.event_field(node).is_some()
    }

    async fn subscribe(
        self: &Arc<Self>,
        node: &SubscriptionNode,
        variables: &serde_json_bytes::Map<serde_json_bytes::ByteString, serde_json_bytes::Value>,
    ) -> Result<EventStream, EventError> {
        let (field, response_name, operation_field) = self
            .event_field(node)
            .ok_or_else(|| EventError::new("event subscription metadata could not be resolved"))?;
        let source_name = field.source.clone();
        let destinations = render_destinations(&field.destinations, operation_field, variables)?;
        let source = self.events.sources.get(&source_name).ok_or_else(|| {
            EventError::new(format!("event source '{source_name}' is not configured"))
        })?;
        let provider = self.events.providers.get(&source.provider).ok_or_else(|| {
            EventError::new(format!(
                "event provider '{}' is not configured",
                source.provider
            ))
        })?;

        let stream = self
            .shared_provider_stream(
                &source.provider,
                provider,
                source,
                &source_name,
                destinations,
            )
            .await?;

        Ok(Box::pin(stream.map(move |event| {
            event.map(|event| decode_graphql_entity(event, &response_name))
        })))
    }

    async fn shared_provider_stream(
        self: &Arc<Self>,
        provider_name: &str,
        provider: &ConfiguredProvider,
        source: &ConfiguredSource,
        source_name: &str,
        destinations: Vec<String>,
    ) -> Result<ProviderEventStream, EventError> {
        let trigger = EventTrigger {
            provider: provider_name.to_string(),
            source: source_name.to_string(),
            destinations: destinations.clone(),
        };
        let access = self.triggers.acquire(trigger.clone()).await?;
        let (receiver, provider_events) = match access {
            TriggerAccess::Shared(receiver) => (receiver, None),
            TriggerAccess::Connect(connection) => {
                let subscription = ProviderSubscription::new(
                    provider_name,
                    source_name,
                    destinations,
                    source.buffer_capacity,
                    self.instance_id,
                );
                match provider.subscribe(source, subscription).await {
                    Ok(provider_events) => {
                        let (sender, receiver) = broadcast::channel(source.buffer_capacity);
                        connection.activate(sender.clone()).await;
                        (receiver, Some((provider_events, sender)))
                    }
                    Err(error) => {
                        connection.fail(error.clone()).await;
                        return Err(error);
                    }
                }
            }
        };
        let shared = provider_events.is_none();
        u64_counter!(
            "apollo.router.operations.subscriptions.events",
            "Total event-backed subscription operations",
            1,
            event.provider.type = provider.type_name(),
            event.source = source_name.to_string(),
            event.trigger.shared = shared
        );

        if let Some((mut provider_events, shared_sender)) = provider_events {
            let trigger_registry = self.triggers.clone();
            let pump_trigger = trigger.clone();
            tokio::spawn(async move {
                let mut idle_check = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! {
                        _ = idle_check.tick() => {
                            if shared_sender.receiver_count() == 0 {
                                break;
                            }
                        }
                        event = provider_events.next() => match event {
                            Some(event) => {
                                let outcome = if event.is_ok() { "received" } else { "error" };
                                u64_counter!(
                                    "apollo.router.operations.subscriptions.events.provider",
                                    "Events and errors received from event providers",
                                    1,
                                    event.provider.name = pump_trigger.provider.clone(),
                                    event.source = pump_trigger.source.clone(),
                                    event.outcome = outcome
                                );
                                let _ = shared_sender.send(Arc::new(event));
                            }
                            None => break,
                        }
                    }
                }
                trigger_registry
                    .remove_active(&pump_trigger, &shared_sender)
                    .await;
            });
        }

        Ok(forward_shared_events(
            receiver,
            source.buffer_capacity,
            trigger,
        ))
    }
}
