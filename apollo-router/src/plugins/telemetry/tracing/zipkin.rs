//! Configuration for zipkin tracing.
use std::sync::LazyLock;

use http::Uri;
use opentelemetry::Key;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::trace::TracerProviderBuilder;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

static DEFAULT_ENDPOINT: LazyLock<Uri> =
    LazyLock::new(|| Uri::from_static("http://127.0.0.1:9411/api/v2/spans"));

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

    fn apply(
        &self,
        builder: TracerProviderBuilder,
        trace: &TracingCommon,
        _spans_config: &Spans,
    ) -> Result<TracerProviderBuilder, BoxError> {
        tracing::info!("configuring Zipkin tracing: {}", self.batch_processor);
        let common: opentelemetry_sdk::trace::Config = trace.into();
        let endpoint = &self.endpoint.to_full_uri(&DEFAULT_ENDPOINT);
        let exporter = opentelemetry_zipkin::ZipkinExporter::builder()
            .with_collector_endpoint(endpoint.to_string())
            .with(
                &common.resource.get(&Key::from(SERVICE_NAME)),
                |builder, _service_name| builder,
            )
            .build()?;

        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(exporter)
                .with_batch_config(self.batch_processor.clone().into())
                .build()
                .filtered(),
        ))
    }
}
