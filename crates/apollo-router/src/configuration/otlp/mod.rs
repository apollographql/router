#[cfg(feature = "otlp-grpcio")]
mod grpcio;
#[cfg(feature = "otlp-http")]
mod http;
#[cfg(feature = "otlp-tonic")]
mod tonic;

#[cfg(feature = "otlp-grpcio")]
pub use self::grpcio::*;
#[cfg(feature = "otlp-http")]
pub use self::http::*;
#[cfg(feature = "otlp-tonic")]
pub use self::tonic::*;
use crate::configuration::ConfigurationError;
use opentelemetry::sdk::resource::Resource;
use opentelemetry::sdk::trace::{Sampler, Tracer};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Otlp {
    Tracing(Option<Tracing>),
    // TODO metrics
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Tracing {
    exporter: Exporter,
    trace_config: Option<TraceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Exporter {
    #[cfg(feature = "otlp-tonic")]
    Tonic(TonicExporter),
    #[cfg(feature = "otlp-grpcio")]
    GrpcIo(GrpcIoExporter),
    #[cfg(feature = "otlp-http")]
    Http(HttpExporter),
}

impl Exporter {
    pub fn exporter(&self) -> Result<opentelemetry_otlp::SpanExporterBuilder, ConfigurationError> {
        match &self {
            #[cfg(feature = "otlp-tonic")]
            Exporter::Tonic(exporter) => Ok(exporter.exporter()?.into()),
            #[cfg(feature = "otlp-grpcio")]
            Exporter::GrpcIo(exporter) => Ok(exporter.exporter()?.into()),
            #[cfg(feature = "otlp-http")]
            Exporter::Http(exporter) => Ok(exporter.exporter()?.into()),
        }
    }

    pub fn exporter_from_env() -> Result<opentelemetry_otlp::SpanExporterBuilder, ConfigurationError>
    {
        match std::env::var("ROUTER_TRACING").as_deref() {
            #[cfg(feature = "otlp-http")]
            Ok("http") => Ok(HttpExporter::exporter_from_env().into()),
            #[cfg(feature = "otlp-tonic")]
            Ok("tonic") => Ok(TonicExporter::exporter_from_env().into()),
            #[cfg(feature = "otlp-grpcio")]
            Ok("grpcio") => Ok(GrpcIoExporter::exporter_from_env().into()),
            Ok(val) => Err(ConfigurationError::InvalidEnvironmentVariable(format!(
                "unrecognized value for ROUTER_TRACING: {}",
                val
            ))),
            Err(e) => Err(ConfigurationError::MissingEnvironmentVariable(format!(
                "could not read ROUTER_TRACING environment variable: {}",
                e
            ))),
        }
    }
}

impl Tracing {
    pub fn tracer(&self) -> Result<Tracer, ConfigurationError> {
        let mut pipeline = opentelemetry_otlp::new_pipeline().tracing();

        pipeline = pipeline.with_exporter(self.exporter.exporter()?);

        if let Some(config) = self.trace_config.as_ref() {
            pipeline = pipeline.with_trace_config(config.trace_config());
        }

        pipeline
            .install_batch(opentelemetry::runtime::Tokio)
            .map_err(ConfigurationError::OtlpTracing)
    }

    pub fn tracer_from_env() -> Result<Tracer, ConfigurationError> {
        let mut pipeline = opentelemetry_otlp::new_pipeline().tracing();

        pipeline = pipeline.with_exporter(Exporter::exporter_from_env()?);

        pipeline
            .install_batch(opentelemetry::runtime::Tokio)
            .map_err(ConfigurationError::OtlpTracing)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ExportConfig {
    pub endpoint: Option<Url>,
    pub protocol: Option<Protocol>,
    pub timeout: Option<u64>,
}

impl ExportConfig {
    pub fn apply<T: WithExportConfig>(&self, mut exporter: T) -> T {
        if let Some(url) = self.endpoint.as_ref() {
            exporter = exporter.with_endpoint(url.as_str());
        }
        if let Some(protocol) = self.protocol {
            exporter = exporter.with_protocol(protocol);
        }
        if let Some(secs) = self.timeout {
            exporter = exporter.with_timeout(Duration::from_secs(secs));
        }
        exporter
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceConfig {
    pub sampler: Option<Sampler>,
    pub max_events_per_span: Option<u32>,
    pub max_attributes_per_span: Option<u32>,
    pub max_links_per_span: Option<u32>,
    pub max_attributes_per_event: Option<u32>,
    pub max_attributes_per_link: Option<u32>,
    pub resource: Option<Resource>,
}

impl TraceConfig {
    pub fn trace_config(&self) -> opentelemetry::sdk::trace::Config {
        let mut trace_config = opentelemetry::sdk::trace::config();
        if let Some(sampler) = self.sampler.clone() {
            trace_config = trace_config.with_sampler(sampler);
        }
        if let Some(n) = self.max_events_per_span {
            trace_config = trace_config.with_max_events_per_span(n);
        }
        if let Some(n) = self.max_attributes_per_span {
            trace_config = trace_config.with_max_attributes_per_span(n);
        }
        if let Some(n) = self.max_links_per_span {
            trace_config = trace_config.with_max_links_per_span(n);
        }
        if let Some(n) = self.max_attributes_per_event {
            trace_config = trace_config.with_max_attributes_per_event(n);
        }
        if let Some(n) = self.max_attributes_per_link {
            trace_config = trace_config.with_max_attributes_per_link(n);
        }
        if let Some(resource) = self.resource.clone() {
            trace_config = trace_config.with_resource(resource);
        }
        trace_config
    }
}
