//! Configuration for datadog tracing.

use std::collections::HashMap;

use lazy_static::lazy_static;
use opentelemetry::sdk;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::Key;
use opentelemetry::Value;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use opentelemetry_semantic_conventions::resource::SERVICE_VERSION;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use super::agent_endpoint;
use super::deser_endpoint;
use super::AgentEndpoint;
use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

lazy_static! {
    static ref SPAN_RESOURCE_NAME_ATTRIBUTE_MAPPING: HashMap<&'static str, &'static str> = {
        let mut map = HashMap::new();
        map.insert("request", "http.route");
        map.insert("supergraph", "graphql.operation.name");
        map.insert("query_planning", "graphql.operation.name");
        map.insert("subgraph", "apollo.subgraph.name");
        map.insert("subgraph_request", "graphql.operation.name");
        map
    };
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The endpoint to send to
    #[serde(deserialize_with = "deser_endpoint")]
    #[schemars(schema_with = "agent_endpoint")]
    pub(crate) endpoint: AgentEndpoint,

    /// batch processor configuration
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,

    /// Enable datadog span mapping for span name and resource name.
    #[serde(default)]
    pub(crate) enable_span_mapping: bool,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace: &Trace) -> Result<Builder, BoxError> {
        tracing::info!("Configuring Datadog tracing: {}", self.batch_processor);
        let url = match &self.endpoint {
            AgentEndpoint::Default(_) => None,
            AgentEndpoint::Url(s) => Some(s),
        };
        let enable_span_mapping = self.enable_span_mapping.then_some(true);
        let trace_config: sdk::trace::Config = trace.into();
        let exporter = opentelemetry_datadog::new_pipeline()
            .with(&url, |builder, e| {
                builder.with_agent_endpoint(e.to_string().trim_end_matches('/'))
            })
            .with(&enable_span_mapping, |builder, _e| {
                builder
                    .with_name_mapping(|span, _model_config| span.name.as_ref())
                    .with_resource_mapping(|span, _model_config| {
                        SPAN_RESOURCE_NAME_ATTRIBUTE_MAPPING
                            .get(span.name.as_ref())
                            .and_then(|key| span.attributes.get(&Key::from_static_str(key)))
                            .and_then(|value| match value {
                                Value::String(value) => Some(value.as_str()),
                                _ => None,
                            })
                            .unwrap_or(span.name.as_ref())
                    })
            })
            .with(
                &trace_config.resource.get(SERVICE_NAME),
                |builder, service_name| {
                    // Datadog exporter incorrectly ignores the service name in the resource
                    // Set it explicitly here
                    builder.with_service_name(service_name.as_str())
                },
            )
            .with_version(
                trace_config
                    .resource
                    .get(SERVICE_VERSION)
                    .expect("cargo version is set as a resource default;qed")
                    .to_string(),
            )
            .with_trace_config(trace_config)
            .build_exporter()?;
        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                .with_batch_config(self.batch_processor.clone().into())
                .build()
                .filtered(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Url;

    use super::*;
    use crate::plugins::telemetry::tracing::AgentDefault;

    #[test]
    fn endpoint_configuration() {
        let config: Config = serde_yaml::from_str("endpoint: default").unwrap();
        assert_eq!(
            AgentEndpoint::Default(AgentDefault::Default),
            config.endpoint
        );

        let config: Config = serde_yaml::from_str("endpoint: collector:1234").unwrap();
        assert_eq!(
            AgentEndpoint::Url(Url::parse("http://collector:1234").unwrap()),
            config.endpoint
        );

        let config: Config = serde_yaml::from_str("endpoint: https://collector:1234").unwrap();
        assert_eq!(
            AgentEndpoint::Url(Url::parse("https://collector:1234").unwrap()),
            config.endpoint
        );
    }
}
