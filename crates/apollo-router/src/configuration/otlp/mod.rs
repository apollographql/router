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
use serde::{Deserialize, Deserializer, Serialize};
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
        let exporter = Exporter::exporter_from_env();
        opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(exporter)
            .install_batch(opentelemetry::runtime::Tokio)
            .map_err(ConfigurationError::OtlpTracing)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ExportConfig {
    #[serde(deserialize_with = "endpoint_url", default)]
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

fn endpoint_url<'de, D>(deserializer: D) -> Result<Option<Url>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut buf = String::deserialize(deserializer)?;

    // support the case of a IP:port endpoint
    if buf.parse::<std::net::SocketAddr>().is_ok() {
        buf = format!("https://{}", buf);
    }

    let mut url = Url::parse(&buf).map_err(serde::de::Error::custom)?;

    // support the case of 'collector:4317' where url parses 'collector'
    // as the scheme instead of the host
    if url.host().is_none() && (url.scheme() != "http" || url.scheme() != "https") {
        buf = format!("https://{}", buf);

        url = Url::parse(&buf).map_err(serde::de::Error::custom)?;
    }

    Ok(Some(url))
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
