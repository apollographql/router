//! Kafka adapter using one instance-unique consumer group per logical trigger.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use rdkafka::ClientConfig;
use rdkafka::Message;
use rdkafka::consumer::CommitMode;
use rdkafka::consumer::Consumer;
use rdkafka::consumer::StreamConsumer;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use super::EventError;
use super::ProviderEvent;
use super::ProviderEventStream;
use crate::configuration::events::EventProviderConfiguration;
use crate::configuration::events::EventSourceConfiguration;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct KafkaConfiguration {
    bootstrap_servers: Vec<String>,
    client_id: Option<String>,
    security: KafkaSecurityConfiguration,
    /// Escape hatch for librdkafka options not modeled above.
    properties: BTreeMap<String, String>,
}

impl Default for KafkaConfiguration {
    fn default() -> Self {
        Self {
            bootstrap_servers: vec!["127.0.0.1:9092".to_string()],
            client_id: None,
            security: KafkaSecurityConfiguration::default(),
            properties: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum KafkaSecurityConfiguration {
    #[default]
    Plaintext,
    Tls {
        #[serde(flatten)]
        tls: KafkaTlsConfiguration,
    },
    SaslPlaintext {
        mechanism: String,
        username: String,
        password: String,
    },
    SaslTls {
        mechanism: String,
        username: String,
        password: String,
        #[serde(flatten)]
        tls: KafkaTlsConfiguration,
    },
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KafkaTlsConfiguration {
    ca_location: Option<PathBuf>,
    certificate_location: Option<PathBuf>,
    key_location: Option<PathBuf>,
    key_password: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct KafkaSourceOptions {
    group_prefix: String,
    topic_mode: KafkaTopicMode,
    #[serde(with = "humantime_serde")]
    session_timeout: Duration,
    #[serde(with = "humantime_serde")]
    heartbeat_interval: Duration,
    #[serde(with = "humantime_serde")]
    max_poll_interval: Duration,
    /// Escape hatch for consumer options. Policy invariants override conflicts.
    properties: BTreeMap<String, String>,
}

impl Default for KafkaSourceOptions {
    fn default() -> Self {
        Self {
            group_prefix: "apollo-router".to_string(),
            topic_mode: KafkaTopicMode::default(),
            session_timeout: Duration::from_secs(45),
            heartbeat_interval: Duration::from_secs(3),
            max_poll_interval: Duration::from_secs(5 * 60),
            properties: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KafkaTopicMode {
    #[default]
    Exact,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn subscribe(
    provider_name: &str,
    config: &KafkaConfiguration,
    options: &KafkaSourceOptions,
    connect_timeout: Duration,
    source_name: &str,
    destinations: Vec<String>,
    buffer_capacity: usize,
    instance_id: uuid::Uuid,
    trigger_hash: u64,
) -> Result<ProviderEventStream, EventError> {
    if destinations.is_empty() || destinations.iter().any(|topic| topic.trim().is_empty()) {
        return Err(EventError::new(format!(
            "Kafka source '{source_name}' must have at least one non-empty topic"
        )));
    }
    if matches!(options.topic_mode, KafkaTopicMode::Exact)
        && destinations.iter().any(|topic| topic.starts_with('^'))
    {
        return Err(EventError::new(format!(
            "Kafka source '{source_name}' exact topic mode does not accept regex topics"
        )));
    }

    let group_id = consumer_group(&options.group_prefix, instance_id, trigger_hash);
    let mut client_config = ClientConfig::new();
    for (key, value) in &config.properties {
        client_config.set(key, value);
    }
    for (key, value) in &options.properties {
        client_config.set(key, value);
    }
    client_config.set("bootstrap.servers", config.bootstrap_servers.join(","));
    if let Some(client_id) = &config.client_id {
        client_config.set("client.id", client_id);
    }
    apply_security(&mut client_config, &config.security);
    // These invariants implement live/on_enqueue/every-instance and override
    // conflicting escape-hatch properties above.
    client_config
        .set("group.id", group_id)
        .set("auto.offset.reset", "latest")
        .set("enable.auto.commit", "false")
        .set("enable.auto.offset.store", "false")
        .set("session.timeout.ms", millis(options.session_timeout))
        .set("heartbeat.interval.ms", millis(options.heartbeat_interval))
        .set("max.poll.interval.ms", millis(options.max_poll_interval));

    let provider_label = provider_name.to_string();
    let consumer: StreamConsumer = tokio::task::spawn_blocking(move || {
        let consumer: StreamConsumer = client_config.create().map_err(|error| {
            EventError::new(format!(
                "could not create Kafka provider '{provider_label}': {error}"
            ))
        })?;
        consumer
            .fetch_metadata(None, connect_timeout)
            .map_err(|error| {
                EventError::new(format!(
                    "could not connect Kafka provider '{provider_label}': {error}"
                ))
            })?;
        Ok::<_, EventError>(consumer)
    })
    .await
    .map_err(|error| EventError::new(format!("Kafka connection task failed: {error}")))??;
    consumer
        .subscribe(&destinations.iter().map(String::as_str).collect::<Vec<_>>())
        .map_err(|error| {
            EventError::new(format!(
                "could not subscribe Kafka source '{source_name}': {error}"
            ))
        })?;

    let (sender, receiver) = tokio::sync::mpsc::channel(buffer_capacity);
    let source_name = source_name.to_string();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sender.closed() => break,
                result = consumer.recv() => match result {
                    Ok(message) => {
                        let Some(payload) = message.payload() else {
                            tracing::warn!(source = %source_name, topic = message.topic(), partition = message.partition(), offset = message.offset(), "ignoring Kafka event with a null payload");
                            if let Err(error) = consumer.commit_message(&message, CommitMode::Async) {
                                tracing::warn!(source = %source_name, %error, "could not commit null Kafka event");
                            }
                            continue;
                        };
                        let event = ProviderEvent {
                            payload: bytes::Bytes::copy_from_slice(payload),
                        };
                        if sender.send(Ok(event)).await.is_err() {
                            break;
                        }
                        if let Err(error) = consumer.commit_message(&message, CommitMode::Async) {
                            tracing::warn!(source = %source_name, %error, "could not commit Kafka event after enqueue");
                        }
                    }
                    Err(error) => {
                        if sender.send(Err(EventError::new(format!(
                            "Kafka source '{source_name}' receive error: {error}"
                        )))).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
        consumer.unsubscribe();
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

fn apply_security(config: &mut ClientConfig, security: &KafkaSecurityConfiguration) {
    match security {
        KafkaSecurityConfiguration::Plaintext => {
            config.set("security.protocol", "plaintext");
        }
        KafkaSecurityConfiguration::Tls { tls } => {
            config.set("security.protocol", "ssl");
            apply_tls(config, tls);
        }
        KafkaSecurityConfiguration::SaslPlaintext {
            mechanism,
            username,
            password,
        } => {
            config
                .set("security.protocol", "sasl_plaintext")
                .set("sasl.mechanism", mechanism)
                .set("sasl.username", username)
                .set("sasl.password", password);
        }
        KafkaSecurityConfiguration::SaslTls {
            mechanism,
            username,
            password,
            tls,
        } => {
            config
                .set("security.protocol", "sasl_ssl")
                .set("sasl.mechanism", mechanism)
                .set("sasl.username", username)
                .set("sasl.password", password);
            apply_tls(config, tls);
        }
    }
}

fn apply_tls(config: &mut ClientConfig, tls: &KafkaTlsConfiguration) {
    if let Some(path) = &tls.ca_location {
        config.set("ssl.ca.location", path.to_string_lossy());
    }
    if let Some(path) = &tls.certificate_location {
        config.set("ssl.certificate.location", path.to_string_lossy());
    }
    if let Some(path) = &tls.key_location {
        config.set("ssl.key.location", path.to_string_lossy());
    }
    if let Some(password) = &tls.key_password {
        config.set("ssl.key.password", password);
    }
}

fn consumer_group(prefix: &str, instance_id: uuid::Uuid, trigger_hash: u64) -> String {
    let prefix = prefix
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .take(180)
        .collect::<String>();
    format!("{prefix}-{instance_id}-{trigger_hash:016x}")
}

fn millis(duration: Duration) -> String {
    duration.as_millis().min(u32::MAX as u128).to_string()
}

fn invalid_provider(provider_name: &str, error: impl std::fmt::Display) -> EventError {
    EventError::new(format!(
        "invalid Kafka provider '{provider_name}' configuration: {error}"
    ))
}

fn invalid_source(source_name: &str, error: impl std::fmt::Display) -> EventError {
    EventError::new(format!(
        "invalid Kafka source '{source_name}' configuration: {error}"
    ))
}

pub(super) fn parse_provider(
    provider_name: &str,
    provider: &EventProviderConfiguration,
) -> Result<KafkaConfiguration, EventError> {
    let config = serde_json::from_value::<KafkaConfiguration>(serde_json::Value::Object(
        provider.config.clone(),
    ))
    .map_err(|error| invalid_provider(provider_name, error))?;
    if config.bootstrap_servers.is_empty() {
        return Err(EventError::new(format!(
            "Kafka provider '{provider_name}' must have at least one bootstrap server"
        )));
    }
    Ok(config)
}

pub(super) fn parse_source(
    source_name: &str,
    source: &EventSourceConfiguration,
) -> Result<KafkaSourceOptions, EventError> {
    serde_json::from_value::<KafkaSourceOptions>(serde_json::Value::Object(
        source.provider_options.clone(),
    ))
    .map_err(|error| invalid_source(source_name, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_security_and_escape_hatch_properties() {
        let config: KafkaConfiguration = serde_json::from_value(serde_json::json!({
            "bootstrap_servers": ["broker-a:9093", "broker-b:9093"],
            "client_id": "router",
            "security": {
                "type": "sasl_tls",
                "mechanism": "SCRAM-SHA-512",
                "username": "router",
                "password": "secret",
                "ca_location": "/etc/ssl/kafka-ca.pem"
            },
            "properties": {"client.rack": "us-west-2a"}
        }))
        .expect("configuration is valid");
        assert_eq!(config.bootstrap_servers.len(), 2);
        assert_eq!(
            config.properties.get("client.rack").map(String::as_str),
            Some("us-west-2a")
        );
    }

    #[test]
    fn creates_stable_bounded_instance_group() {
        let instance = uuid::Uuid::nil();
        let group = consumer_group("tenant/product updates", instance, 42);
        assert!(group.starts_with("tenant-product-updates-"));
        assert!(group.len() <= 255);
        assert_eq!(
            group,
            consumer_group("tenant/product updates", instance, 42)
        );
    }
}
