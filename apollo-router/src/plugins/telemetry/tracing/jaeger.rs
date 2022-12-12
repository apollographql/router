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
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
// Can't use #[serde(deny_unknown_fields)] because we're using flatten for endpoint
pub(crate) struct Config {
    #[serde(flatten)]
    #[schemars(schema_with = "endpoint_schema")]
    pub(crate) endpoint: Endpoint,

    #[serde(default)]
    #[schemars(default)]
    pub(crate) batch_processor: Option<BatchProcessorConfig>,
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
                o.object.as_mut().unwrap().properties.insert(
                    "batch_processor".to_string(),
                    BatchProcessorConfig::json_schema(gen),
                );
            }
        });

    schema.into()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Endpoint {
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
        tracing::info!(
            "configuring Jaeger tracing: {}",
            self.batch_processor.as_ref().cloned().unwrap_or_default()
        );
        match &self.endpoint {
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
                let exporter = opentelemetry_jaeger::new_agent_pipeline()
                    .with_trace_config(trace_config.into())
                    .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                    .with(&socket, |b, s| b.with_endpoint(s))
                    .build_async_agent_exporter(opentelemetry::runtime::Tokio)?;
                Ok(builder.with_span_processor(
                    BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                        .with_batch_config(
                            self.batch_processor
                                .as_ref()
                                .cloned()
                                .unwrap_or_default()
                                .into(),
                        )
                        .build()
                        .filtered(),
                ))
            }
            Endpoint::Collector {
                endpoint,
                username,
                password,
                ..
            } => {
                // We are waiting for a release of https://github.com/open-telemetry/opentelemetry-rust/issues/894
                // Until that time we need to wrap a tracer provider with Jeager in.
                let tracer_provider = opentelemetry_jaeger::new_collector_pipeline()
                    .with_trace_config(trace_config.into())
                    .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                    .with(username, |b, u| b.with_username(u))
                    .with(password, |b, p| b.with_password(p))
                    .with_endpoint(&endpoint.to_string())
                    .with_reqwest()
                    .with_batch_processor_config(
                        self.batch_processor
                            .as_ref()
                            .cloned()
                            .unwrap_or_default()
                            .into(),
                    )
                    .build_batch(opentelemetry::runtime::Tokio)?;
                Ok(builder.with_span_processor(DelegateSpanProcessor { tracer_provider }))
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
