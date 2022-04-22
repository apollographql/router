//! Configuration for zipkin tracing.
use crate::plugins::telemetry::config::{GenericWith, Trace};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower::BoxError;
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Config {
    #[serde(flatten)]
    #[schemars(with = "String")]
    pub endpoint: Endpoint,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    Agent {
        #[schemars(with = "String", default = "default_agent_endpoint")]
        endpoint: AgentEndpoint,
    },
    Collector {
        endpoint: Url,
    },
}

fn default_agent_endpoint() -> &'static str {
    "default"
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", untagged)]
pub enum AgentEndpoint {
    Default(AgentDefault),
    Socket(SocketAddr),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDefault {
    Default,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Zipkin tracing");
        let exporter = match &self.endpoint {
            Endpoint::Agent { endpoint } => {
                let socket = match endpoint {
                    AgentEndpoint::Default(_) => None,
                    AgentEndpoint::Socket(s) => Some(s),
                };
                opentelemetry_zipkin::new_pipeline()
                    .with_trace_config(trace_config.into())
                    .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                    .with(&socket, |b, s| b.with_service_address(*(*s)))
                    .init_exporter()?
            }
            Endpoint::Collector { endpoint } => opentelemetry_zipkin::new_pipeline()
                .with_trace_config(trace_config.into())
                .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                .with_collector_endpoint(&endpoint.to_string())
                .init_exporter()?,
        };
        Ok(builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio))
    }
}
