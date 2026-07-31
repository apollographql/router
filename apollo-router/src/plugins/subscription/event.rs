//! Provider-neutral event source integration for federated subscriptions.

use std::collections::HashMap;
use std::fmt;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::pin::Pin;
use std::sync::Arc;

use apollo_compiler::executable;
use futures::Stream;
use futures::StreamExt;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::configuration::events::EventsConfiguration;
use crate::error::Error;
use crate::graphql;
use crate::plugins::subscription::SubscriptionTaskParams;
use crate::query_planner::OperationKind;
use crate::query_planner::subscription::SubscriptionNode;
use crate::services::FetchResponse;
use crate::services::fetch::SubscriptionRequest;
use crate::services::subgraph::BoxGqlStream;
use crate::spec::Schema;

const SUBSCRIBE_DIRECTIVE_NAME: &str = "event__subscribe";

mod kafka;
mod nats_core;
mod nats_jetstream;
mod redis_pubsub;

/// Opaque provider position, exposed only to telemetry and provider settlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventCursor(pub(crate) String);

/// A message before format decoding.
#[derive(Debug)]
pub(crate) struct ProviderEvent {
    pub(crate) payload: bytes::Bytes,
    pub(crate) metadata: HashMap<String, String>,
    pub(crate) cursor: Option<EventCursor>,
}

#[derive(Debug)]
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

#[derive(Clone, Debug)]
struct EventField {
    source: String,
    destinations: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct EventCatalog {
    fields: HashMap<(String, String), EventField>,
}

impl EventCatalog {
    fn from_schema(schema: &Schema) -> Self {
        let supergraph = schema.supergraph_schema();
        let graph_names = supergraph
            .get_enum("join__Graph")
            .map(|graph_enum| {
                graph_enum
                    .values
                    .iter()
                    .filter_map(|(enum_name, value)| {
                        let name = value
                            .directives
                            .get("join__graph")?
                            .specified_argument_by_name("name")?
                            .as_str()?;
                        Some((enum_name.to_string(), name.to_string()))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        let mut fields = HashMap::new();
        let root = schema.root_operation_name(OperationKind::Subscription);
        let Some(subscription) = supergraph.get_object(root) else {
            return Self { fields };
        };

        for (field_name, definition) in &subscription.fields {
            for directive in definition.directives.get_all("join__directive") {
                let is_event = directive
                    .argument_by_name("name", supergraph)
                    .ok()
                    .and_then(|value| value.as_str())
                    == Some(SUBSCRIBE_DIRECTIVE_NAME);
                if !is_event {
                    continue;
                }

                let Some(args) = directive
                    .argument_by_name("args", supergraph)
                    .ok()
                    .and_then(|value| value.as_object())
                else {
                    tracing::error!(field = %field_name, "event subscription directive has no args");
                    continue;
                };
                let source = args.iter().find_map(|(name, value)| {
                    (name.as_str() == "source")
                        .then(|| value.as_str())
                        .flatten()
                });
                let destinations = args
                    .iter()
                    .find_map(|(name, value)| {
                        (name.as_str() == "destinations")
                            .then(|| value.as_list())
                            .flatten()
                    })
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(ToString::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let graphs = directive
                    .argument_by_name("graphs", supergraph)
                    .ok()
                    .and_then(|value| value.as_list());

                let (Some(source), Some(graphs)) = (source, graphs) else {
                    tracing::error!(field = %field_name, "event subscription directive is incomplete");
                    continue;
                };
                for graph in graphs {
                    if let Some(graph) = graph.as_enum()
                        && let Some(service_name) = graph_names.get(graph.as_str())
                    {
                        fields.insert(
                            (service_name.clone(), field_name.to_string()),
                            EventField {
                                source: source.to_string(),
                                destinations: destinations.clone(),
                            },
                        );
                    }
                }
            }
        }
        Self { fields }
    }
}

/// Owns schema metadata and provider adapters for one live router pipeline.
pub(crate) struct EventRuntime {
    configuration: EventsConfiguration,
    catalog: EventCatalog,
    nats_clients: Mutex<HashMap<String, async_nats::Client>>,
    instance_id: uuid::Uuid,
}

impl EventRuntime {
    pub(crate) fn new(schema: Arc<Schema>, configuration: EventsConfiguration) -> Self {
        Self {
            configuration,
            catalog: EventCatalog::from_schema(&schema),
            nats_clients: Mutex::new(HashMap::new()),
            instance_id: uuid::Uuid::new_v4(),
        }
    }

    fn event_field(&self, node: &SubscriptionNode) -> Option<(&EventField, String)> {
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
        Some((event, response_name))
    }

    pub(crate) fn is_event_subscription(&self, node: &SubscriptionNode) -> bool {
        self.event_field(node).is_some()
    }

    pub(crate) fn subscribe(
        self: Arc<Self>,
        request: SubscriptionRequest,
    ) -> futures::future::BoxFuture<'static, Result<FetchResponse, BoxError>> {
        Box::pin(async move {
            let Some((field, response_name)) = self.event_field(&request.subscription_node) else {
                return Ok(event_error(
                    "event subscription metadata could not be resolved",
                ));
            };
            let source_name = field.source.clone();
            let destinations = field.destinations.clone();
            let Some(source) = self.configuration.sources.get(&source_name) else {
                return Ok(event_error(format!(
                    "event source '{source_name}' is not configured"
                )));
            };
            let Some(provider) = self.configuration.providers.get(&source.provider) else {
                return Ok(event_error(format!(
                    "event provider '{}' is not configured",
                    source.provider
                )));
            };
            let Some(policy) = self.configuration.policies.get(&source.policy) else {
                return Ok(event_error(format!(
                    "event policy '{}' is not configured",
                    source.policy
                )));
            };

            let stream = match self
                .provider_stream(
                    &source.provider,
                    provider,
                    source,
                    &source_name,
                    destinations,
                    policy.buffer.capacity.get(),
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

    async fn provider_stream(
        &self,
        provider_name: &str,
        provider: &crate::configuration::events::EventProviderConfiguration,
        source: &crate::configuration::events::EventSourceConfiguration,
        source_name: &str,
        destinations: Vec<String>,
        buffer_capacity: usize,
    ) -> Result<ProviderEventStream, EventError> {
        match provider.r#type.as_str() {
            "nats_core" => {
                nats_core::subscribe(
                    self,
                    provider_name,
                    provider,
                    source,
                    source_name,
                    destinations,
                    buffer_capacity,
                )
                .await
            }
            "nats_jetstream" => {
                nats_jetstream::subscribe(
                    self,
                    provider_name,
                    provider,
                    source,
                    source_name,
                    destinations,
                    buffer_capacity,
                )
                .await
            }
            "redis_pubsub" => {
                redis_pubsub::subscribe(
                    provider_name,
                    provider,
                    source,
                    source_name,
                    destinations,
                    buffer_capacity,
                )
                .await
            }
            "kafka" => {
                let mut hasher = DefaultHasher::new();
                source_name.hash(&mut hasher);
                destinations.hash(&mut hasher);
                kafka::subscribe(
                    provider_name,
                    provider,
                    source,
                    source_name,
                    destinations,
                    buffer_capacity,
                    self.instance_id,
                    hasher.finish(),
                )
                .await
            }
            provider_type => Err(EventError::new(format!(
                "event provider type '{provider_type}' is not supported by this build"
            ))),
        }
    }
}

fn decode_graphql_entity(event: ProviderEvent, response_name: &str) -> graphql::Response {
    let _metadata = event.metadata;
    let _cursor = event.cursor;
    match serde_json::from_slice::<Value>(&event.payload) {
        Ok(value @ Value::Object(_)) if value.get("__typename").is_some() => {
            let mut data = Map::new();
            data.insert(ByteString::from(response_name), value);
            graphql::Response::builder()
                .data(Value::Object(data))
                .build()
        }
        Ok(Value::Object(_)) => graphql::Response::builder()
            .error(
                Error::builder()
                    .message("event payload is missing required '__typename'")
                    .extension_code("EVENT_DECODE_ERROR")
                    .build(),
            )
            .build(),
        Ok(_) => graphql::Response::builder()
            .error(
                Error::builder()
                    .message("event payload must be a JSON object")
                    .extension_code("EVENT_DECODE_ERROR")
                    .build(),
            )
            .build(),
        Err(error) => graphql::Response::builder()
            .error(
                Error::builder()
                    .message(format!("event payload is not valid JSON: {error}"))
                    .extension_code("EVENT_DECODE_ERROR")
                    .build(),
            )
            .build(),
    }
}

async fn install_stream(
    request: SubscriptionRequest,
    stream: BoxGqlStream,
) -> Result<FetchResponse, BoxError> {
    let Some(subscription_config) = request.subscription_config else {
        return Ok(event_error("subscription support is not enabled"));
    };
    let Some(mut handle) = request.subscription_handle else {
        return Ok(event_error("no subscription handle was provided"));
    };
    let Some(configuration_sender) = handle.subscription_conf_tx.take() else {
        return Ok(event_error(
            "no subscription configuration sender was provided",
        ));
    };

    let (stream_sender, stream_receiver) = mpsc::channel(1);
    stream_sender
        .send(stream)
        .await
        .map_err(|error| EventError::new(error.to_string()))?;
    configuration_sender
        .send(SubscriptionTaskParams {
            client_sender: request.sender,
            subscription_handle: handle,
            subscription_config,
            stream_rx: stream_receiver.into(),
        })
        .await
        .map_err(|error| EventError::new(error.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_entity_under_response_field() {
        let response = decode_graphql_entity(
            ProviderEvent {
                payload: bytes::Bytes::from_static(br#"{"__typename":"Product","id":"1"}"#),
                metadata: HashMap::new(),
                cursor: None,
            },
            "productUpdated",
        );
        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "productUpdated": {"__typename": "Product", "id": "1"}
            }))
        );
    }

    #[test]
    fn rejects_entity_without_typename() {
        let response = decode_graphql_entity(
            ProviderEvent {
                payload: bytes::Bytes::from_static(br#"{"id":"1"}"#),
                metadata: HashMap::new(),
                cursor: None,
            },
            "productUpdated",
        );
        assert_eq!(
            response.errors[0].extension_code(),
            Some("EVENT_DECODE_ERROR".to_string())
        );
    }
}
