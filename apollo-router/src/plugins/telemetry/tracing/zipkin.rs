//! Configuration for zipkin tracing.
use crate::plugins::telemetry::config::{GenericWith, Trace};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::BoxError;
use url::Url;

use super::{deser_endpoint, AgentEndpoint};

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
        #[serde(deserialize_with = "deser_endpoint")]
        endpoint: AgentEndpoint,
    },
    Collector {
        endpoint: Url,
    },
}

fn default_agent_endpoint() -> &'static str {
    "default"
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Zipkin tracing");
        let exporter = match &self.endpoint {
            Endpoint::Agent { endpoint } => {
                let socket = match endpoint {
                    AgentEndpoint::Default(_) => None,
                    AgentEndpoint::Url(u) => {
                        let socket_addr = u.socket_addrs(|| None)?.pop().ok_or_else(|| {
                            format!("cannot resolve url ({}) for zipkin agent", u)
                        })?;
                        Some(socket_addr)
                    }
                };
                opentelemetry_zipkin::new_pipeline()
                    .with_trace_config(trace_config.into())
                    .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                    .with(&socket, |b, s| b.with_service_address(*s))
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
