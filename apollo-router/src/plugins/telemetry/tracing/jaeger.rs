use crate::plugins::telemetry::config::{GenericWith, Trace};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower::BoxError;
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde()]
pub struct Config {
    #[serde(flatten)]
    pub endpoint: Endpoint,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Endpoint {
    Agent {
        #[schemars(with = "String", default = "default_agent_endpoint")]
        endpoint: AgentEndpoint,
    },
    Collector {
        endpoint: Url,
        username: Option<String>,
        password: Option<String>,
    },
}

fn default_agent_endpoint() -> &'static str {
    "default"
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub enum AgentEndpoint {
    Default(AgentDefault),
    Socket(SocketAddr),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum AgentDefault {
    Default,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Jaeger tracing");
        let exporter = match &self.endpoint {
            Endpoint::Agent { endpoint } => {
                let socket = match endpoint {
                    AgentEndpoint::Default(_) => None,
                    AgentEndpoint::Socket(s) => Some(s),
                };
                opentelemetry_jaeger::new_pipeline()
                    .with_trace_config(trace_config.into())
                    .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                    .with(&socket, |b, s| b.with_agent_endpoint(s))
                    .init_async_exporter(opentelemetry::runtime::Tokio)?
            }
            Endpoint::Collector {
                endpoint,
                username,
                password,
            } => opentelemetry_jaeger::new_pipeline()
                .with_trace_config(trace_config.into())
                .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                .with(username, |b, u| b.with_collector_username(u))
                .with(password, |b, p| b.with_collector_password(p))
                .with_collector_endpoint(&endpoint.to_string())
                .init_async_exporter(opentelemetry::runtime::Tokio)?,
        };

        Ok(builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio))
    }
}
