//! Provider-neutral event source integration for federated subscriptions.

use std::collections::HashMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::executable;
use futures::Stream;
use futures::StreamExt;
use serde_json_bytes::Value;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::configuration::events::EventsConfiguration;
use crate::error::Error;
use crate::graphql;
use crate::plugins::subscription::SubscriptionTaskParams;
use crate::plugins::subscription::fetch::install_subscription_task;
use crate::plugins::subscription::fetch::subscription_admission_error;
use crate::query_planner::subscription::SubscriptionNode;
use crate::services::FetchResponse;
use crate::services::fetch::SubscriptionRequest;
use crate::services::subgraph::BoxGqlStream;
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
mod template;

use catalog::EventCatalog;
use catalog::EventField;
use decoder::decode_graphql_entity;
use fanout::EventTrigger;
use fanout::TriggerRegistry;
use fanout::forward_shared_events;
use providers::ConfiguredEvents;
use providers::ConfiguredProvider;
use providers::ConfiguredSource;
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
            triggers: Arc::new(Mutex::new(HashMap::new())),
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

    pub(crate) fn is_event_subscription(&self, node: &SubscriptionNode) -> bool {
        self.event_field(node).is_some()
    }

    pub(crate) fn subscribe(
        self: Arc<Self>,
        request: SubscriptionRequest,
    ) -> futures::future::BoxFuture<'static, Result<FetchResponse, BoxError>> {
        Box::pin(async move {
            if let Some(error) = subscription_admission_error(request.subscription_config.as_ref())
            {
                return Ok(error);
            }
            let Some((field, response_name, operation_field)) =
                self.event_field(&request.subscription_node)
            else {
                return Ok(event_error(
                    "event subscription metadata could not be resolved",
                ));
            };
            let source_name = field.source.clone();
            let destinations = match render_destinations(
                &field.destinations,
                operation_field,
                &request.variables.variables,
            ) {
                Ok(destinations) => destinations,
                Err(error) => return Ok(event_error(error.to_string())),
            };
            let Some(source) = self.events.sources.get(&source_name) else {
                return Ok(event_error(format!(
                    "event source '{source_name}' is not configured"
                )));
            };
            let Some(provider) = self.events.providers.get(&source.provider) else {
                return Ok(event_error(format!(
                    "event provider '{}' is not configured",
                    source.provider
                )));
            };

            let stream = match self
                .shared_provider_stream(
                    &source.provider,
                    provider,
                    source,
                    &source_name,
                    destinations,
                )
                .await
            {
                Ok(stream) => stream,
                Err(error) => return Ok(event_error(error.to_string())),
            };
            let gql_stream: BoxGqlStream = Box::pin(stream.map(move |event| {
                match event {
                    Ok(event) => decode_graphql_entity(event, &response_name),
                    Err(error) => graphql::Response::builder()
                        .error(
                            Error::builder()
                                .message(error.to_string())
                                .extension_code("EVENT_STREAM_ERROR")
                                .build(),
                        )
                        .build(),
                }
            }));
            install_stream(request, gql_stream).await
        })
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
        let mut triggers = self.triggers.lock().await;
        if let Some(sender) = triggers.get(&trigger) {
            u64_counter!(
                "apollo.router.operations.subscriptions.events",
                "Total event-backed subscription operations",
                1,
                event.provider.type = provider.type_name(),
                event.source = source_name.to_string(),
                event.trigger.shared = true
            );
            return Ok(forward_shared_events(
                sender.subscribe(),
                source.buffer_capacity,
                trigger,
            ));
        }

        let mut provider_events = provider
            .subscribe(
                provider_name,
                source,
                source_name,
                destinations,
                source.buffer_capacity,
                self.instance_id,
            )
            .await?;
        let (shared_sender, first_receiver) = broadcast::channel(source.buffer_capacity);
        u64_counter!(
            "apollo.router.operations.subscriptions.events",
            "Total event-backed subscription operations",
            1,
            event.provider.type = provider.type_name(),
            event.source = source_name.to_string(),
            event.trigger.shared = false
        );
        triggers.insert(trigger.clone(), shared_sender.clone());
        drop(triggers);

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
            let mut triggers = trigger_registry.lock().await;
            if triggers
                .get(&pump_trigger)
                .is_some_and(|current| current.same_channel(&shared_sender))
            {
                triggers.remove(&pump_trigger);
            }
        });

        Ok(forward_shared_events(
            first_receiver,
            source.buffer_capacity,
            trigger,
        ))
    }
}

async fn install_stream(
    request: SubscriptionRequest,
    stream: BoxGqlStream,
) -> Result<FetchResponse, BoxError> {
    let Some(subscription_config) = request.subscription_config else {
        return Ok(event_error("subscription support is not enabled"));
    };
    let Some(handle) = request.subscription_handle else {
        return Ok(event_error("no subscription handle was provided"));
    };

    let (stream_sender, stream_receiver) = mpsc::channel(1);
    stream_sender
        .send(stream)
        .await
        .map_err(|error| EventError::new(error.to_string()))?;
    if let Err(response) = install_subscription_task(SubscriptionTaskParams {
        client_sender: request.sender,
        subscription_handle: handle,
        subscription_config,
        stream_rx: stream_receiver.into(),
    })
    .await
    {
        return Ok(response);
    }
    Ok((Value::default(), Vec::new()))
}

fn event_error(message: impl Into<String>) -> FetchResponse {
    (
        Value::default(),
        vec![
            Error::builder()
                .message(message.into())
                .extension_code("EVENT_SUBSCRIPTION_ERROR")
                .build(),
        ],
    )
}
