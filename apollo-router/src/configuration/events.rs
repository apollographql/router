//! Provider-neutral configuration for event-backed graph capabilities.
//!
//! The outer configuration is intentionally stable: providers own physical
//! connections, sources are logical names referenced by a graph, and policies
//! describe delivery behavior. Provider- and format-specific configuration is
//! kept behind explicitly delegated `config` objects so adding an implementation
//! does not change the public envelope.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

/// Configuration for event-backed graph capabilities.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventsConfiguration {
    /// Physical event-provider connections, keyed by a stable local name.
    pub(crate) providers: BTreeMap<String, EventProviderConfiguration>,
    /// Logical event sources referenced from composed graph metadata.
    pub(crate) sources: BTreeMap<String, EventSourceConfiguration>,
    /// Reusable delivery policies.
    pub(crate) policies: BTreeMap<String, EventPolicyConfiguration>,
}

impl EventsConfiguration {
    pub(crate) fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.sources.is_empty() && self.policies.is_empty()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        for (name, provider) in &self.providers {
            validate_name("provider", name)?;
            if provider.r#type.trim().is_empty() {
                return Err(format!(
                    "event provider '{name}' must have a non-empty type"
                ));
            }
        }

        for name in self.policies.keys() {
            validate_name("policy", name)?;
        }

        for (name, source) in &self.sources {
            validate_name("source", name)?;
            if !self.providers.contains_key(&source.provider) {
                return Err(format!(
                    "event source '{name}' references unknown provider '{}'",
                    source.provider
                ));
            }
            if !self.policies.contains_key(&source.policy) {
                return Err(format!(
                    "event source '{name}' references unknown policy '{}'",
                    source.policy
                ));
            }
            if source.format.r#type.trim().is_empty() {
                return Err(format!(
                    "event source '{name}' must have a non-empty format type"
                ));
            }
        }

        Ok(())
    }
}

fn validate_name(kind: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!("event {kind} names must not be empty"));
    }
    if name != name.trim() {
        return Err(format!(
            "event {kind} name '{name}' must not have leading or trailing whitespace"
        ));
    }
    Ok(())
}

/// A physical connection to an event provider.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventProviderConfiguration {
    /// Provider implementation identifier, such as `nats_core`, `nats_jetstream`,
    /// `redis_pubsub`, or `kafka`.
    #[serde(rename = "type")]
    pub(crate) r#type: String,
    /// Provider-owned configuration, validated by the selected provider.
    #[serde(default)]
    pub(crate) config: Map<String, Value>,
    /// Connection lifecycle behavior shared by all providers.
    #[serde(default)]
    pub(crate) lifecycle: ProviderLifecycleConfiguration,
}

/// Common provider lifecycle behavior.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderLifecycleConfiguration {
    /// Maximum duration allowed for the initial connection attempt.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) connect_timeout: Duration,
}

impl Default for ProviderLifecycleConfiguration {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// A logical event source referenced by graph metadata.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventSourceConfiguration {
    /// Name of the provider connection used by this source.
    pub(crate) provider: String,
    /// Name of the reusable delivery policy used by this source.
    pub(crate) policy: String,
    /// Wire-format decoder for events received from this source.
    #[serde(default)]
    pub(crate) format: EventFormatConfiguration,
    /// Provider-owned per-source options such as consumer configuration.
    #[serde(default)]
    pub(crate) provider_options: Map<String, Value>,
}

/// Event wire-format configuration.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventFormatConfiguration {
    /// Format implementation identifier. The initial implementation is `graphql_entity`.
    #[serde(rename = "type")]
    pub(crate) r#type: String,
    /// Format-owned decoding and mapping configuration.
    pub(crate) config: Map<String, Value>,
}

impl Default for EventFormatConfiguration {
    fn default() -> Self {
        Self {
            r#type: "graphql_entity".to_string(),
            config: Map::new(),
        }
    }
}

/// Reusable event-delivery behavior.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventPolicyConfiguration {
    pub(crate) delivery: EventDeliveryConfiguration,
    pub(crate) buffer: EventBufferConfiguration,
    pub(crate) distribution: EventDistributionConfiguration,
    pub(crate) ordering: EventOrderingConfiguration,
}

/// Delivery and acknowledgement contract.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventDeliveryConfiguration {
    /// Live-edge delivery. Events are not retained by Router for client replay.
    Live {
        #[serde(default)]
        start: EventStartConfiguration,
        #[serde(default)]
        acknowledgement: EventAcknowledgementConfiguration,
    },
}

impl Default for EventDeliveryConfiguration {
    fn default() -> Self {
        Self::Live {
            start: EventStartConfiguration::default(),
            acknowledgement: EventAcknowledgementConfiguration::default(),
        }
    }
}

/// Position from which a provider begins a live subscription.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventStartConfiguration {
    /// Receive only events produced after the trigger starts.
    #[default]
    Latest,
}

/// Point at which a provider event can be acknowledged.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventAcknowledgementConfiguration {
    /// Acknowledge after an event enters Router's bounded local fan-out queue.
    #[default]
    OnEnqueue,
}

/// Bounded buffering and overflow behavior.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventBufferConfiguration {
    /// Maximum number of events retained for each local subscriber.
    pub(crate) capacity: NonZeroUsize,
    /// Behavior when a local subscriber falls behind this capacity.
    pub(crate) overflow: EventOverflowConfiguration,
}

impl Default for EventBufferConfiguration {
    fn default() -> Self {
        Self {
            capacity: NonZeroUsize::new(128).expect("128 is non-zero"),
            overflow: EventOverflowConfiguration::DropOldest,
        }
    }
}

/// Behavior when a local event buffer is full.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventOverflowConfiguration {
    /// Discard the oldest queued event and retain the newly received event.
    #[default]
    DropOldest,
}

/// Distribution topology expected from the provider adapter.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventDistributionConfiguration {
    /// Every Router process with a matching trigger must receive every event.
    #[default]
    EveryRouterInstance,
}

/// Ordering contract exposed by the first-pass implementation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventOrderingConfiguration {
    /// Preserve only the ordering supplied by the provider for one trigger.
    #[default]
    Provider,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_implicit_first_version() {
        let config: EventsConfiguration = serde_yaml::from_str(
            r#"
providers:
  production-events:
    type: nats_core
    config:
      servers: [nats://localhost:4222]
sources:
  product-updates:
    provider: production-events
    policy: live-updates
policies:
  live-updates:
    delivery:
      type: live
      start: { type: latest }
      acknowledgement: { type: on_enqueue }
    distribution: { type: every_router_instance }
"#,
        )
        .expect("configuration is valid YAML");

        config.validate().expect("references are valid");
        assert_eq!(config.providers["production-events"].r#type, "nats_core");
        assert_eq!(
            config.sources["product-updates"].format.r#type,
            "graphql_entity"
        );
        assert_eq!(config.policies["live-updates"].buffer.capacity.get(), 128);
    }

    #[test]
    fn rejects_unknown_references() {
        let config: EventsConfiguration = serde_yaml::from_str(
            r#"
sources:
  product-updates:
    provider: missing-provider
    policy: missing-policy
"#,
        )
        .expect("configuration is valid YAML");

        assert_eq!(
            config.validate().unwrap_err(),
            "event source 'product-updates' references unknown provider 'missing-provider'"
        );
    }

    #[test]
    fn rejects_an_explicit_version_field() {
        let error = serde_yaml::from_str::<EventsConfiguration>(
            r#"
version: 1
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field `version`"));
    }

    #[test]
    fn provider_extension_config_is_always_an_object() {
        let error = serde_yaml::from_str::<EventsConfiguration>(
            r#"
providers:
  production-events:
    type: test
    config: not-an-object
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid type"));
    }
}
