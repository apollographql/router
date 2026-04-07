//! Configuration for Zipkin tracing.
//!
//! # Deprecation Notice
//!
//! The native Zipkin exporter is deprecated and will be removed in the next major release
//! of the Router. Zipkin supports OTLP ingestion, and OpenTelemetry is deprecating native
//! Zipkin exporters in favor of OTLP. Users should migrate to the OTLP exporter instead.
//!
//! # Known Limitations
//!
//! The upstream `opentelemetry-zipkin` crate (v0.31) does not currently support setting
//! the service name on the Zipkin `localEndpoint`. This means traces exported to Zipkin
//! will not have a service name associated with them.
//!
//! See: <https://github.com/open-telemetry/opentelemetry-rust/issues/381>
use std::sync::LazyLock;

use http::Uri;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use opentelemetry_zipkin::ZipkinExporter;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::reload::tracing::TracingBuilder;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::NamedSpanExporter;
use crate::plugins::telemetry::tracing::NamedTokioRuntime;
use crate::plugins::telemetry::tracing::SpanProcessorExt;

const OTEL_EXPORTER_ZIPKIN_ENDPOINT: &str = "OTEL_EXPORTER_ZIPKIN_ENDPOINT";

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

impl Config {
    /// Apply environment variable overrides.
    /// Supports `OTEL_EXPORTER_ZIPKIN_ENDPOINT`.
    fn endpoint_with_env_override(&self) -> Result<Uri, BoxError> {
        if let Ok(endpoint) = std::env::var(OTEL_EXPORTER_ZIPKIN_ENDPOINT) {
            endpoint.parse::<Uri>().map_err(|e| {
                format!(
                    "invalid URI in {}: '{}': {}",
                    OTEL_EXPORTER_ZIPKIN_ENDPOINT, endpoint, e
                )
                .into()
            })
        } else {
            Ok(self.endpoint.to_full_uri(&DEFAULT_ENDPOINT))
        }
    }
}

impl TracingConfigurator for Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.tracing.zipkin
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError> {
        tracing::info!("configuring Zipkin tracing: {}", self.batch_processor);
        let endpoint = self.endpoint_with_env_override()?;
        // TODO: The upstream opentelemetry-zipkin crate (v0.31) removed support for setting
        // service_name on the localEndpoint. Track upstream fix or implement workaround.
        // See: https://github.com/open-telemetry/opentelemetry-rust/issues/381
        let exporter = ZipkinExporter::builder()
            .with_collector_endpoint(endpoint.to_string())
            .build()?;

        let named_exporter = NamedSpanExporter::new(exporter, "zipkin");
        builder.with_span_processor(
            BatchSpanProcessor::builder(named_exporter, NamedTokioRuntime::new("zipkin-tracing"))
                .with_batch_config(self.batch_processor.clone().with_env_overrides()?.into())
                .build()
                .filtered(),
        );
        Ok(())
    }
}
