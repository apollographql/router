//! Configuration and connection state shared by NATS Core and JetStream.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::OnceCell;

use super::EventError;
use crate::configuration::events::EventProviderConfiguration;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct NatsConfiguration {
    servers: Vec<String>,
    name: Option<String>,
    auth: Option<NatsAuthConfiguration>,
    tls: Option<NatsTlsConfiguration>,
    #[serde(with = "humantime_serde")]
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

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum NatsAuthConfiguration {
    UserPassword { username: String, password: String },
    Token { token: String },
    Credentials { path: PathBuf },
    Nkey { seed: String },
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct NatsTlsConfiguration {
    required: bool,
    ca_file: Option<PathBuf>,
    client_certificate: Option<PathBuf>,
    client_key: Option<PathBuf>,
}

pub(super) struct NatsProvider {
    configuration: NatsConfiguration,
    connect_timeout: Duration,
    client: OnceCell<async_nats::Client>,
}

impl NatsProvider {
    pub(super) fn parse(
        provider_name: &str,
        provider: &EventProviderConfiguration,
    ) -> Result<Self, EventError> {
        let configuration = serde_json::from_value::<NatsConfiguration>(serde_json::Value::Object(
            provider.config.clone(),
        ))
        .map_err(|error| invalid_configuration(provider_name, error))?;
        validate_tls(&configuration)?;
        configuration
            .servers
            .iter()
            .try_for_each(|server| server.parse::<async_nats::ServerAddr>().map(|_| ()))
            .map_err(|error| invalid_configuration(provider_name, error))?;
        Ok(Self {
            configuration,
            connect_timeout: provider.lifecycle.connect_timeout,
            client: OnceCell::new(),
        })
    }

    pub(super) async fn client(
        &self,
        provider_name: &str,
    ) -> Result<async_nats::Client, EventError> {
        self.client
            .get_or_try_init(|| self.connect(provider_name))
            .await
            .cloned()
    }

    async fn connect(&self, provider_name: &str) -> Result<async_nats::Client, EventError> {
        let configuration = &self.configuration;
        let mut options =
            async_nats::ConnectOptions::new().ping_interval(configuration.ping_interval);
        if configuration.retry_on_initial_connect {
            options = options.retry_on_initial_connect();
        }
        if let Some(name) = &configuration.name {
            options = options.name(name);
        }
        if let Some(auth) = &configuration.auth {
            options = match auth {
                NatsAuthConfiguration::UserPassword { username, password } => {
                    options.user_and_password(username.clone(), password.clone())
                }
                NatsAuthConfiguration::Token { token } => options.token(token.clone()),
                NatsAuthConfiguration::Credentials { path } => {
                    options.credentials_file(path).await.map_err(|error| {
                        EventError::new(format!("invalid NATS credentials: {error}"))
                    })?
                }
                NatsAuthConfiguration::Nkey { seed } => options.nkey(seed.clone()),
            };
        }
        if let Some(tls) = &configuration.tls {
            options = options.require_tls(tls.required);
            if let Some(path) = &tls.ca_file {
                options = options.add_root_certificates(path.clone());
            }
            if let (Some(certificate), Some(key)) = (&tls.client_certificate, &tls.client_key) {
                options = options.add_client_certificate(certificate.clone(), key.clone());
            }
        }

        let servers = configuration
            .servers
            .iter()
            .map(|server| server.parse())
            .collect::<Result<Vec<async_nats::ServerAddr>, _>>()
            .map_err(|error| invalid_configuration(provider_name, error))?;
        tokio::time::timeout(self.connect_timeout, options.connect(servers))
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
            })
    }
}

fn validate_tls(configuration: &NatsConfiguration) -> Result<(), EventError> {
    if let Some(tls) = &configuration.tls {
        match (&tls.client_certificate, &tls.client_key) {
            (Some(_), Some(_)) | (None, None) => {}
            _ => {
                return Err(EventError::new(
                    "NATS TLS client_certificate and client_key must be configured together",
                ));
            }
        }
    }
    Ok(())
}

fn invalid_configuration(provider_name: &str, error: impl std::fmt::Display) -> EventError {
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
}
