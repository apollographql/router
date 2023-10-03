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
    static ref DEFAULT_ENDPOINT: Uri = Uri::from_static("http://localhost:9411/api/v2/spans");
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The endpoint to send to
    pub(crate) endpoint: UriEndpoint,

    /// Batch processor configuration
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,
}

impl TracingConfigurator for Config {
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
