//! NATS Core adapter. Every Router process uses ordinary subscriptions so each
//! process receives every matching message; queue groups are intentionally not
//! used because they would violate `every_router_instance` distribution.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tokio_stream::wrappers::ReceiverStream;

use super::EventError;
use super::EventRuntime;
use super::ProviderEvent;
use super::ProviderEventStream;
use crate::configuration::events::EventProviderConfiguration;
use crate::configuration::events::EventSourceConfiguration;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NatsConfiguration {
    servers: Vec<String>,
    name: Option<String>,
    auth: Option<NatsAuthConfiguration>,
    tls: Option<NatsTlsConfiguration>,
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    ping_interval: Duration,
    retry_on_initial_connect: bool,
}

impl Default for NatsConfiguration {
    fn default() -> Self {
        Self {
            servers: vec!["nats://127.0.0.1:4222".to_string()],
            name: None,
            auth: None,
            tls: None,
            ping_interval: Duration::from_secs(60),
            retry_on_initial_connect: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum NatsAuthConfiguration {
    UserPassword { username: String, password: String },
    Token { token: String },
    Credentials { path: PathBuf },
    Nkey { seed: String },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NatsTlsConfiguration {
    required: bool,
    ca_file: Option<PathBuf>,
    client_certificate: Option<PathBuf>,
    client_key: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NatsCoreSourceOptions {}

pub(super) async fn subscribe(
    runtime: &EventRuntime,
    provider_name: &str,
    provider: &EventProviderConfiguration,
    source: &EventSourceConfiguration,
    source_name: &str,
    destinations: Vec<String>,
    buffer_capacity: usize,
) -> Result<ProviderEventStream, EventError> {
    let config: NatsConfiguration =
        serde_json::from_value(serde_json::Value::Object(provider.config.clone()))
            .map_err(|error| invalid_config(provider_name, error))?;
    let _: NatsCoreSourceOptions =
        serde_json::from_value(serde_json::Value::Object(source.provider_options.clone()))
            .map_err(|error| {
                EventError::new(format!("invalid NATS Core source '{source_name}': {error}"))
            })?;
    if destinations.is_empty() {
        return Err(EventError::new(format!(
            "NATS Core source '{source_name}' must have at least one subject"
        )));
    }

    let client = client(runtime, provider_name, provider, &config).await?;
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
            let metadata = nats_metadata(&message);
            let event = ProviderEvent {
                payload: message.payload,
                metadata,
                cursor: None,
            };
            if sender.send(Ok(event)).await.is_err() {
                break;
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

async fn client(
    runtime: &EventRuntime,
    provider_name: &str,
    provider: &EventProviderConfiguration,
    config: &NatsConfiguration,
) -> Result<async_nats::Client, EventError> {
    let mut clients = runtime.nats_clients.lock().await;
    if let Some(client) = clients.get(provider_name) {
        return Ok(client.clone());
    }

    let mut options = async_nats::ConnectOptions::new().ping_interval(config.ping_interval);
    if config.retry_on_initial_connect {
        options = options.retry_on_initial_connect();
    }
    if let Some(name) = &config.name {
        options = options.name(name);
    }
    if let Some(auth) = &config.auth {
        options = match auth {
            NatsAuthConfiguration::UserPassword { username, password } => {
                options.user_and_password(username.clone(), password.clone())
            }
            NatsAuthConfiguration::Token { token } => options.token(token.clone()),
            NatsAuthConfiguration::Credentials { path } => options
                .credentials_file(path)
                .await
                .map_err(|error| EventError::new(format!("invalid NATS credentials: {error}")))?,
            NatsAuthConfiguration::Nkey { seed } => options.nkey(seed.clone()),
        };
    }
    if let Some(tls) = &config.tls {
        options = options.require_tls(tls.required);
        if let Some(path) = &tls.ca_file {
            options = options.add_root_certificates(path.clone());
        }
        match (&tls.client_certificate, &tls.client_key) {
            (Some(certificate), Some(key)) => {
                options = options.add_client_certificate(certificate.clone(), key.clone());
            }
            (None, None) => {}
            _ => {
                return Err(EventError::new(
                    "NATS TLS client_certificate and client_key must be configured together",
                ));
            }
        }
    }

    let servers = config
        .servers
        .iter()
        .map(|server| server.parse())
        .collect::<Result<Vec<async_nats::ServerAddr>, _>>()
        .map_err(|error| invalid_config(provider_name, error))?;
    let connected =
        tokio::time::timeout(provider.lifecycle.connect_timeout, options.connect(servers))
            .await
            .map_err(|_| {
                EventError::new(format!(
                    "timed out connecting NATS provider '{provider_name}'"
                ))
            })?
            .map_err(|error| {
                EventError::new(format!(
                    "could not connect NATS provider '{provider_name}': {error}"
                ))
            })?;
    clients.insert(provider_name.to_string(), connected.clone());
    Ok(connected)
}

fn nats_metadata(message: &async_nats::Message) -> HashMap<String, String> {
    let mut metadata = HashMap::from([("subject".to_string(), message.subject.to_string())]);
    if let Some(reply) = &message.reply {
        metadata.insert("reply".to_string(), reply.to_string());
    }
    metadata
}

fn invalid_config(provider_name: &str, error: impl std::fmt::Display) -> EventError {
    EventError::new(format!(
        "invalid NATS provider '{provider_name}' configuration: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_future_ready_auth_and_tls_shape() {
        let config: NatsConfiguration = serde_json::from_value(serde_json::json!({
            "servers": ["tls://nats.example.com:4222"],
            "name": "router",
            "auth": {"type": "credentials", "path": "/run/secrets/nats.creds"},
            "tls": {"required": true, "ca_file": "/etc/ssl/nats-ca.pem"},
            "ping_interval": "30s"
        }))
        .expect("configuration is valid");
        assert_eq!(config.ping_interval, Duration::from_secs(30));
        assert!(matches!(
            config.auth,
            Some(NatsAuthConfiguration::Credentials { .. })
        ));
    }

    #[test]
    fn rejects_queue_group_source_option() {
        let error = serde_json::from_value::<NatsCoreSourceOptions>(serde_json::json!({
            "queue_group": "shared"
        }))
        .expect_err("queue groups violate every_router_instance");
        assert!(error.to_string().contains("unknown field"));
    }
}
