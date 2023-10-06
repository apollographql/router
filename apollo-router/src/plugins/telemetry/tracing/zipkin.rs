//! Configuration for zipkin tracing.
use http::Uri;
use lazy_static::lazy_static;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

lazy_static! {
    static ref DEFAULT_ENDPOINT: Uri = Uri::from_static("http://127.0.0.1:9411/api/v2/spans");
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable zipkin
    pub(crate) enabled: bool,

    /// The endpoint to send to
    #[serde(default)]
    pub(crate) endpoint: UriEndpoint,

    /// Batch processor configuration
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,
}

impl TracingConfigurator for Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::info!("configuring Zipkin tracing: {}", self.batch_processor);

        let exporter = opentelemetry_zipkin::new_pipeline()
            .with_trace_config(trace_config.into())
            .with_service_name(trace_config.service_name.clone())
            .with(&self.endpoint.to_uri(&DEFAULT_ENDPOINT), |b, endpoint| {
                b.with_collector_endpoint(endpoint.to_string())
            })
            .init_exporter()?;

        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                .with_batch_config(self.batch_processor.clone().into())
                .build()
                .filtered(),
        ))
    }
}
