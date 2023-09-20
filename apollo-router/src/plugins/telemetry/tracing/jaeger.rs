//! Configuration for jaeger tracing.
use std::fmt::Debug;

use http::Uri;
use lazy_static::lazy_static;
use opentelemetry::runtime;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::endpoint::SocketEndpoint;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

lazy_static! {
    static ref DEFAULT_ENDPOINT: Uri = Uri::from_static("http://localhost:14268/api/traces");
}
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum Config {
    Agent {
        /// Enable Jaeger
        enabled: bool,

        /// Agent configuration
        #[serde(default)]
        agent: AgentConfig,

        /// Batch processor configuration
        #[serde(default)]
        batch_processor: BatchProcessorConfig,
    },
    Collector {
        /// Enable Jaeger
        enabled: bool,

        /// Collector configuration
        #[serde(default)]
        collector: CollectorConfig,

        /// Batch processor configuration
        #[serde(default)]
        batch_processor: BatchProcessorConfig,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct AgentConfig {
    /// The endpoint to send to
    endpoint: SocketEndpoint,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CollectorConfig {
    /// The endpoint to send reports to
    endpoint: UriEndpoint,
    /// The optional username
    username: Option<String>,
    /// The optional password
    password: Option<String>,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        match &self {
            Config::Agent {
                enabled,
                agent,
                batch_processor,
            } if *enabled => {
                tracing::info!("Configuring Jaeger tracing: {}", batch_processor);
                let exporter = opentelemetry_jaeger::new_agent_pipeline()
                    .with_trace_config(trace_config.into())
                    .with_service_name(trace_config.service_name.clone())
                    .with(&agent.endpoint.to_socket(), |b, s| b.with_endpoint(s))
                    .build_async_agent_exporter(opentelemetry::runtime::Tokio)?;
                Ok(builder.with_span_processor(
                    BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                        .with_batch_config(batch_processor.clone().into())
                        .build()
                        .filtered(),
                ))
            }
            Config::Collector {
                enabled,
                collector,
                batch_processor,
            } if *enabled => {
                tracing::info!("Configuring Jaeger tracing: {}", batch_processor);

                let exporter = opentelemetry_jaeger::new_collector_pipeline()
                    .with_trace_config(trace_config.into())
                    .with_service_name(trace_config.service_name.clone())
                    .with(&collector.username, |b, u| b.with_username(u))
                    .with(&collector.password, |b, p| b.with_password(p))
                    .with(
                        &collector
                            .endpoint
                            .to_uri(&DEFAULT_ENDPOINT)
                            // https://github.com/open-telemetry/opentelemetry-rust/issues/1280 Default jaeger endpoint for collector looks incorrect
                            .or_else(|| Some(DEFAULT_ENDPOINT.clone())),
                        |b, p| b.with_endpoint(p.to_string()),
                    )
                    .with_reqwest()
                    .with_batch_processor_config(batch_processor.clone().into())
                    .build_collector_exporter::<runtime::Tokio>()?;
                Ok(builder.with_span_processor(
                    BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                        .with_batch_config(batch_processor.clone().into())
                        .build(),
                ))
            }
            _ => Ok(builder),
        }
    }
}
