use std::collections::BTreeMap;
use std::time::Duration;

use super::EventError;
use super::ProviderEventStream;
use super::kafka;
use super::nats;
use super::nats_core;
use super::nats_jetstream;
use super::redis_pubsub;
use crate::configuration::events::EventProviderConfiguration;
use crate::configuration::events::EventSourceConfiguration;
use crate::configuration::events::EventsConfiguration;

pub(super) struct ConfiguredEvents {
    pub(super) providers: BTreeMap<String, ConfiguredProvider>,
    pub(super) sources: BTreeMap<String, ConfiguredSource>,
}

impl ConfiguredEvents {
    pub(super) fn parse(configuration: EventsConfiguration) -> Result<Self, EventError> {
        configuration.validate().map_err(EventError::new)?;
        let providers = configuration
            .providers
            .iter()
            .map(|(name, provider)| {
                ConfiguredProvider::parse(name, provider).map(|provider| (name.clone(), provider))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let sources = configuration
            .sources
            .iter()
            .map(|(name, source)| {
                let provider = providers.get(&source.provider).ok_or_else(|| {
                    EventError::new(format!(
                        "event source '{name}' references unknown provider '{}'",
                        source.provider
                    ))
                })?;
                let policy = configuration.policies.get(&source.policy).ok_or_else(|| {
                    EventError::new(format!(
                        "event source '{name}' references unknown policy '{}'",
                        source.policy
                    ))
                })?;
                ConfiguredSource::parse(name, source, provider, policy.buffer.capacity.get())
                    .map(|source| (name.clone(), source))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(Self { providers, sources })
    }
}

pub(super) enum ConfiguredProvider {
    NatsCore(nats::NatsProvider),
    NatsJetStream(nats::NatsProvider),
    RedisPubSub {
        configuration: redis_pubsub::RedisPubSubConfiguration,
        connect_timeout: Duration,
    },
    Kafka {
        configuration: kafka::KafkaConfiguration,
        connect_timeout: Duration,
    },
}

/// Provider-neutral context for opening a stream for one resolved event trigger.
///
/// Keeping the trigger identity and runtime controls together prevents provider
/// adapters from growing a long, order-sensitive list of scalar arguments as
/// more providers or cross-provider controls are added.
pub(super) struct ProviderSubscription<'a> {
    pub(super) provider_name: &'a str,
    pub(super) source_name: &'a str,
    pub(super) destinations: Vec<String>,
    pub(super) buffer_capacity: usize,
    pub(super) instance_id: uuid::Uuid,
}

impl<'a> ProviderSubscription<'a> {
    pub(super) fn new(
        provider_name: &'a str,
        source_name: &'a str,
        destinations: Vec<String>,
        buffer_capacity: usize,
        instance_id: uuid::Uuid,
    ) -> Self {
        Self {
            provider_name,
            source_name,
            destinations,
            buffer_capacity,
            instance_id,
        }
    }
}

impl ConfiguredProvider {
    fn parse(
        provider_name: &str,
        provider: &EventProviderConfiguration,
    ) -> Result<Self, EventError> {
        match provider.r#type.as_str() {
            "nats_core" => Ok(Self::NatsCore(nats::NatsProvider::parse(
                provider_name,
                provider,
            )?)),
            "nats_jetstream" => Ok(Self::NatsJetStream(nats::NatsProvider::parse(
                provider_name,
                provider,
            )?)),
            "redis_pubsub" => Ok(Self::RedisPubSub {
                configuration: redis_pubsub::parse_provider(provider_name, provider)?,
                connect_timeout: provider.lifecycle.connect_timeout,
            }),
            "kafka" => Ok(Self::Kafka {
                configuration: kafka::parse_provider(provider_name, provider)?,
                connect_timeout: provider.lifecycle.connect_timeout,
            }),
            provider_type => Err(EventError::new(format!(
                "event provider '{provider_name}' has unsupported type '{provider_type}'"
            ))),
        }
    }

    pub(super) fn type_name(&self) -> &'static str {
        match self {
            Self::NatsCore(_) => "nats_core",
            Self::NatsJetStream(_) => "nats_jetstream",
            Self::RedisPubSub { .. } => "redis_pubsub",
            Self::Kafka { .. } => "kafka",
        }
    }

    pub(super) async fn subscribe(
        &self,
        source: &ConfiguredSource,
        subscription: ProviderSubscription<'_>,
    ) -> Result<ProviderEventStream, EventError> {
        match (self, &source.options) {
            (Self::NatsCore(provider), SourceOptions::NatsCore(options)) => {
                nats_core::subscribe(provider, options, subscription).await
            }
            (Self::NatsJetStream(provider), SourceOptions::NatsJetStream(options)) => {
                nats_jetstream::subscribe(provider, options, subscription).await
            }
            (
                Self::RedisPubSub {
                    configuration,
                    connect_timeout,
                },
                SourceOptions::RedisPubSub(options),
            ) => {
                redis_pubsub::subscribe(configuration, options, *connect_timeout, subscription)
                    .await
            }
            (
                Self::Kafka {
                    configuration,
                    connect_timeout,
                },
                SourceOptions::Kafka(options),
            ) => kafka::subscribe(configuration, options, *connect_timeout, subscription).await,
            _ => Err(EventError::new(format!(
                "event source '{}' does not match provider type '{}'",
                subscription.source_name,
                self.type_name()
            ))),
        }
    }
}

pub(super) struct ConfiguredSource {
    pub(super) provider: String,
    pub(super) buffer_capacity: usize,
    options: SourceOptions,
}

impl ConfiguredSource {
    fn parse(
        source_name: &str,
        source: &EventSourceConfiguration,
        provider: &ConfiguredProvider,
        buffer_capacity: usize,
    ) -> Result<Self, EventError> {
        if source.format.r#type != "graphql_entity" {
            return Err(EventError::new(format!(
                "event source '{source_name}' has unsupported format type '{}'",
                source.format.r#type
            )));
        }
        if !source.format.config.is_empty() {
            return Err(EventError::new(format!(
                "event source '{source_name}' has unsupported graphql_entity format options"
            )));
        }
        let options = match provider {
            ConfiguredProvider::NatsCore(_) => {
                SourceOptions::NatsCore(nats_core::parse_source(source_name, source)?)
            }
            ConfiguredProvider::NatsJetStream(_) => {
                SourceOptions::NatsJetStream(nats_jetstream::parse_source(source_name, source)?)
            }
            ConfiguredProvider::RedisPubSub { .. } => {
                SourceOptions::RedisPubSub(redis_pubsub::parse_source(source_name, source)?)
            }
            ConfiguredProvider::Kafka { .. } => {
                SourceOptions::Kafka(kafka::parse_source(source_name, source)?)
            }
        };
        Ok(Self {
            provider: source.provider.clone(),
            buffer_capacity,
            options,
        })
    }
}

enum SourceOptions {
    NatsCore(nats_core::NatsCoreSourceOptions),
    NatsJetStream(nats_jetstream::JetStreamSourceOptions),
    RedisPubSub(redis_pubsub::RedisPubSubSourceOptions),
    Kafka(kafka::KafkaSourceOptions),
}
