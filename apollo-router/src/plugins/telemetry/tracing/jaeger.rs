//! Configuration for jaeger tracing.
use std::time::Duration;

use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use url::Url;

use super::deser_endpoint;
use super::AgentEndpoint;
use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::TracingConfigurator;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
// Can't use #[serde(deny_unknown_fields)] because we're using flatten for endpoint
pub struct Config {
    #[serde(flatten)]
    #[schemars(schema_with = "endpoint_schema")]
    pub endpoint: Endpoint,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub scheduled_delay: Option<Duration>,
}

// This is needed because of the use of flatten.
fn endpoint_schema(gen: &mut SchemaGenerator) -> Schema {
    let mut schema: SchemaObject = <Endpoint>::json_schema(gen).into();

    schema
        .subschemas
        .as_mut()
        .unwrap()
        .one_of
        .as_mut()
        .unwrap()
        .iter_mut()
        .for_each(|s| {
            if let Schema::Object(o) = s {
                o.object
                    .as_mut()
                    .unwrap()
                    .properties
                    .insert("scheduled_delay".to_string(), String::json_schema(gen));
            }
        });

    schema.into()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Endpoint {
    Agent {
        #[schemars(with = "String", default = "default_agent_endpoint")]
        #[serde(deserialize_with = "deser_endpoint")]
        endpoint: AgentEndpoint,
    },
    Collector {
        #[schemars(with = "String")]
        endpoint: Url,
        username: Option<String>,
        password: Option<String>,
    },
}
fn default_agent_endpoint() -> &'static str {
    "default"
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Jaeger tracing");
        let exporter = match &self.endpoint {
            Endpoint::Agent { endpoint } => {
                let socket = match endpoint {
                    AgentEndpoint::Default(_) => None,
                    AgentEndpoint::Url(u) => {
                        let socket_addr = u.socket_addrs(|| None)?.pop().ok_or_else(|| {
                            format!("cannot resolve url ({}) for jaeger agent", u)
                        })?;
                        Some(socket_addr)
                    }
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

        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                .with(&self.scheduled_delay, |b, d| b.with_scheduled_delay(*d))
                .build(),
        ))
    }
}
