//! Configuration for zipkin tracing.
use std::sync::LazyLock;

use http::Uri;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::otel::named_runtime_channel::NamedTokioRuntime;
use crate::plugins::telemetry::reload::builder::TracingBuilder;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

static DEFAULT_ENDPOINT: LazyLock<Uri> =
    LazyLock::new(|| Uri::from_static("http://127.0.0.1:9411/api/v2/spans"));

#[derive(Debug, Clone, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "ZipkinConfig")]
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
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.tracing.zipkin
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(&self, builder: &mut TracingBuilder) -> Result<(), BoxError> {
        tracing::info!("configuring Zipkin tracing: {}", self.batch_processor);
        let common: opentelemetry_sdk::trace::Config = builder.tracing_common().into();
        let endpoint = &self.endpoint.to_full_uri(&DEFAULT_ENDPOINT);
        let exporter = opentelemetry_zipkin::new_pipeline()
            .with_collector_endpoint(endpoint.to_string())
            .with(
                &common.resource.get(SERVICE_NAME.into()),
                |builder, service_name| {
                    // Zipkin exporter incorrectly ignores the service name in the resource
                    // Set it explicitly here
                    builder.with_service_name(service_name.as_str())
                },
            )
            .with_trace_config(common)
            .init_exporter()?;

        builder.with_span_processor(
            BatchSpanProcessor::builder(exporter, NamedTokioRuntime::new("zipkin-tracing"))
                .with_batch_config(self.batch_processor.clone().into())
                .build()
                .filtered(),
        );
        Ok(())
    }
}
