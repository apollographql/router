//! Redis Pub/Sub adapter with explicit regular, pattern, and sharded modes.

use std::sync::Arc;
use std::time::Duration;

use fred::clients::SubscriberClient;
use fred::interfaces::ClientLike;
use fred::interfaces::EventInterface;
use fred::interfaces::PubsubInterface;
use fred::types::Builder;
use fred::types::config::Config as RedisConfig;
use fred::types::config::ReconnectPolicy;
use fred::types::config::TlsConfig;
use fred::types::config::TlsHostMapping;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use super::EventError;
use super::ProviderEvent;
use super::ProviderEventStream;
use crate::configuration::TlsClient;
use crate::configuration::events::EventProviderConfiguration;
use crate::configuration::events::EventSourceConfiguration;
use crate::services::subgraph::http::generate_tls_client_config;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RedisPubSubConfiguration {
    url: String,
    username: Option<String>,
    password: Option<String>,
    tls: Option<TlsClient>,
    reconnect: RedisReconnectConfiguration,
}

impl Default for RedisPubSubConfiguration {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379".to_string(),
            username: None,
            password: None,
            tls: None,
            reconnect: RedisReconnectConfiguration::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RedisReconnectConfiguration {
    Constant {
        #[serde(default)]
        max_attempts: u32,
        #[serde(with = "humantime_serde")]
        delay: Duration,
    },
    Linear {
        #[serde(default)]
        max_attempts: u32,
        #[serde(with = "humantime_serde")]
        delay: Duration,
        #[serde(with = "humantime_serde")]
        max_delay: Duration,
    },
    Exponential {
        #[serde(default)]
        max_attempts: u32,
        #[serde(with = "humantime_serde")]
        min_delay: Duration,
        #[serde(with = "humantime_serde")]
        max_delay: Duration,
        multiplier: u32,
    },
}

impl Default for RedisReconnectConfiguration {
    fn default() -> Self {
        Self::Exponential {
            max_attempts: 0,
            min_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            multiplier: 2,
        }
    }
}

impl RedisReconnectConfiguration {
    fn to_fred(&self) -> ReconnectPolicy {
        match self {
            Self::Constant {
                max_attempts,
                delay,
            } => ReconnectPolicy::new_constant(*max_attempts, millis(*delay)),
            Self::Linear {
                max_attempts,
                delay,
                max_delay,
            } => ReconnectPolicy::new_linear(*max_attempts, millis(*max_delay), millis(*delay)),
            Self::Exponential {
                max_attempts,
                min_delay,
                max_delay,
                multiplier,
            } => ReconnectPolicy::new_exponential(
                *max_attempts,
                millis(*min_delay),
                millis(*max_delay),
                *multiplier,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RedisPubSubMode {
    #[default]
    Channel,
    Pattern,
    Sharded,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RedisPubSubSourceOptions {
    mode: RedisPubSubMode,
}

pub(super) async fn subscribe(
    provider_name: &str,
    config: &RedisPubSubConfiguration,
    source_options: &RedisPubSubSourceOptions,
    connect_timeout: Duration,
    source_name: &str,
    destinations: Vec<String>,
    buffer_capacity: usize,
) -> Result<ProviderEventStream, EventError> {
    if destinations.is_empty() {
        return Err(EventError::new(format!(
            "Redis Pub/Sub source '{source_name}' must have at least one destination"
        )));
    }

    let subscriber = build_client(provider_name, config, connect_timeout, buffer_capacity).await?;
    let mut messages = subscriber.message_rx();
    let subscription_manager = subscriber.manage_subscriptions();
    match source_options.mode {
        RedisPubSubMode::Channel => subscriber.subscribe(destinations).await,
        RedisPubSubMode::Pattern => subscriber.psubscribe(destinations).await,
        RedisPubSubMode::Sharded => subscriber.ssubscribe(destinations).await,
    }
    .map_err(|error| {
        EventError::new(format!(
            "could not subscribe Redis Pub/Sub source '{source_name}': {error}"
        ))
    })?;

    let (sender, receiver) = tokio::sync::mpsc::channel(buffer_capacity);
    let source_name = source_name.to_string();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sender.closed() => break,
                result = messages.recv() => match result {
                    Ok(message) => {
                        let Some(payload) = message.value.into_bytes() else {
                            if sender.send(Err(EventError::new(format!(
                                "Redis Pub/Sub source '{source_name}' received a non-byte payload"
                            )))).await.is_err() {
                                break;
                            }
                            continue;
                        };
                        if sender.send(Ok(ProviderEvent {
                            payload,
                        })).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::warn!(source = %source_name, dropped = count, "Redis Pub/Sub local receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        subscription_manager.abort();
        let _ = subscriber.quit().await;
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

async fn build_client(
    provider_name: &str,
    config: &RedisPubSubConfiguration,
    connect_timeout: Duration,
    buffer_capacity: usize,
) -> Result<SubscriberClient, EventError> {
    let mut redis = RedisConfig::from_url(&config.url)
        .map_err(|error| invalid_provider(provider_name, error))?;
    if let Some(username) = &config.username {
        redis.username = Some(username.clone());
    }
    if let Some(password) = &config.password {
        redis.password = Some(password.clone());
    }
    if let Some(tls) = &config.tls {
        let roots = tls
            .create_certificate_store()
            .transpose()
            .map_err(|error| invalid_provider(provider_name, error))?;
        let client_config =
            generate_tls_client_config(roots, tls.client_authentication.as_ref().map(Arc::as_ref))
                .map_err(|error| invalid_provider(provider_name, error))?;
        redis.tls = Some(TlsConfig {
            connector: fred::types::config::TlsConnector::Rustls(tokio_rustls::TlsConnector::from(
                Arc::new(client_config),
            )),
            hostnames: TlsHostMapping::None,
        });
    }

    let mut builder = Builder::from_config(redis);
    builder
        .set_policy(config.reconnect.to_fred())
        .set_performance_config(fred::types::config::PerformanceConfig {
            broadcast_channel_capacity: buffer_capacity,
            ..Default::default()
        });
    let subscriber = builder
        .build_subscriber_client()
        .map_err(|error| invalid_provider(provider_name, error))?;
    tokio::time::timeout(connect_timeout, subscriber.init())
        .await
        .map_err(|_| {
            EventError::new(format!(
                "timed out connecting Redis Pub/Sub provider '{provider_name}'"
            ))
        })?
        .map_err(|error| {
            EventError::new(format!(
                "could not connect Redis Pub/Sub provider '{provider_name}': {error}"
            ))
        })?;
    Ok(subscriber)
}

fn millis(duration: Duration) -> u32 {
    duration.as_millis().min(u32::MAX as u128) as u32
}

fn invalid_provider(provider_name: &str, error: impl std::fmt::Display) -> EventError {
    EventError::new(format!(
        "invalid Redis Pub/Sub provider '{provider_name}' configuration: {error}"
    ))
}

fn invalid_source(source_name: &str, error: impl std::fmt::Display) -> EventError {
    EventError::new(format!(
        "invalid Redis Pub/Sub source '{source_name}' configuration: {error}"
    ))
}

pub(super) fn parse_provider(
    provider_name: &str,
    provider: &EventProviderConfiguration,
) -> Result<RedisPubSubConfiguration, EventError> {
    let config = serde_json::from_value::<RedisPubSubConfiguration>(serde_json::Value::Object(
        provider.config.clone(),
    ))
    .map_err(|error| invalid_provider(provider_name, error))?;
    RedisConfig::from_url(&config.url).map_err(|error| invalid_provider(provider_name, error))?;
    Ok(config)
}

pub(super) fn parse_source(
    source_name: &str,
    source: &EventSourceConfiguration,
) -> Result<RedisPubSubSourceOptions, EventError> {
    serde_json::from_value::<RedisPubSubSourceOptions>(serde_json::Value::Object(
        source.provider_options.clone(),
    ))
    .map_err(|error| invalid_source(source_name, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auth_tls_and_reconnect_shape() {
        let certificate = include_str!("../../../configuration/testdata/server.crt");
        let config: RedisPubSubConfiguration = serde_json::from_value(serde_json::json!({
            "url": "rediss://redis.example.com:6379",
            "username": "router",
            "password": "secret",
            "tls": {"certificate_authorities": certificate},
            "reconnect": {
                "type": "exponential",
                "max_attempts": 0,
                "min_delay": "100ms",
                "max_delay": "30s",
                "multiplier": 2
            }
        }))
        .expect("configuration is valid");
        assert_eq!(config.username.as_deref(), Some("router"));
        assert!(matches!(
            config.reconnect,
            RedisReconnectConfiguration::Exponential { .. }
        ));
    }

    #[test]
    fn parses_each_subscription_mode() {
        for mode in ["channel", "pattern", "sharded"] {
            let options: RedisPubSubSourceOptions =
                serde_json::from_value(serde_json::json!({"mode": mode})).expect("mode is valid");
            assert_eq!(format!("{:?}", options.mode).to_lowercase(), mode);
        }
    }
}
