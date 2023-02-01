//! Configuration for jaeger tracing.
use std::fmt::Debug;

use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::sdk::trace::Span;
use opentelemetry::sdk::trace::SpanProcessor;
use opentelemetry::sdk::trace::TracerProvider;
use opentelemetry::trace::TraceResult;
use opentelemetry::Context;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use url::Url;

use super::agent_endpoint;
use super::deser_endpoint;
use super::AgentEndpoint;
use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum Config {
    Agent {
        /// Agent configuration
        agent: AgentConfig,

        /// Batch processor configuration
        #[serde(default)]
        batch_processor: BatchProcessorConfig,
    },
    Collector {
        /// Collector configuration
        collector: CollectorConfig,

        /// Batch processor configuration
        #[serde(default)]
        batch_processor: BatchProcessorConfig,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentConfig {
    /// The endpoint to send to
    #[schemars(schema_with = "agent_endpoint")]
    #[serde(deserialize_with = "deser_endpoint")]
    endpoint: AgentEndpoint,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectorConfig {
    /// The endpoint to send reports to
    endpoint: Url,
    /// The optional username
    username: Option<String>,
    /// The optional password
    password: Option<String>,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        match &self {
            Config::Agent {
                agent,
                batch_processor,
            } => {
                tracing::info!("configuring Jaeger tracing: {}", batch_processor);
                let socket = match &agent.endpoint {
                    AgentEndpoint::Default(_) => None,
                    AgentEndpoint::Url(u) => {
                        let socket_addr = u
                            .socket_addrs(|| None)?
                            .pop()
                            .ok_or_else(|| format!("cannot resolve url ({u}) for jaeger agent"))?;
                        Some(socket_addr)
                    }
                };
                let exporter = opentelemetry_jaeger::new_agent_pipeline()
                    .with_trace_config(trace_config.into())
                    .with_service_name(trace_config.service_name.clone())
                    .with(&socket, |b, s| b.with_endpoint(s))
                    .build_async_agent_exporter(opentelemetry::runtime::Tokio)?;
                Ok(builder.with_span_processor(
                    BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                        .with_batch_config(batch_processor.clone().into())
                        .build()
                        .filtered(),
                ))
            }
            Config::Collector {
                collector,
                batch_processor,
            } => {
                tracing::info!("configuring Jaeger tracing: {}", batch_processor);
                // We are waiting for a release of https://github.com/open-telemetry/opentelemetry-rust/issues/894
                // Until that time we need to wrap a tracer provider with Jeager in.
                let tracer_provider = opentelemetry_jaeger::new_collector_pipeline()
                    .with_trace_config(trace_config.into())
                    .with_service_name(trace_config.service_name.clone())
                    .with(&collector.username, |b, u| b.with_username(u))
                    .with(&collector.password, |b, p| b.with_password(p))
                    .with_endpoint(&collector.endpoint.to_string())
                    .with_reqwest()
                    .with_batch_processor_config(batch_processor.clone().into())
                    .build_batch(opentelemetry::runtime::Tokio)?;
                Ok(builder
                    .with_span_processor(DelegateSpanProcessor { tracer_provider }.filtered()))
            }
        }
    }
}

#[derive(Debug)]
struct DelegateSpanProcessor {
    tracer_provider: TracerProvider,
}

impl SpanProcessor for DelegateSpanProcessor {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.tracer_provider.span_processors()[0].on_start(span, cx)
    }

    fn on_end(&self, span: SpanData) {
        self.tracer_provider.span_processors()[0].on_end(span)
    }

    fn force_flush(&self) -> TraceResult<()> {
        self.tracer_provider.span_processors()[0].force_flush()
    }

    fn shutdown(&mut self) -> TraceResult<()> {
        // It's safe to not call shutdown as dropping tracer_provider will cause shutdown to happen separately.
        Ok(())
    }
}
