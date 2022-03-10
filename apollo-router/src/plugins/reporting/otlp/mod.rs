#[cfg(feature = "otlp-grpc")]
mod grpc;
#[cfg(feature = "otlp-http")]
mod http;

#[cfg(feature = "otlp-grpc")]
pub use self::grpc::*;
#[cfg(feature = "otlp-http")]
pub use self::http::*;
use super::TraceConfig;
use crate::configuration::ConfigurationError;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Otlp {
    Tracing(Option<Tracing>),
    // TODO metrics
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Tracing {
    exporter: Exporter,
    trace_config: Option<TraceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Exporter {
    #[cfg(feature = "otlp-grpc")]
    Grpc(Option<GrpcExporter>),
    #[cfg(feature = "otlp-http")]
    Http(Option<HttpExporter>),
}

impl Exporter {
    pub fn exporter(&self) -> Result<opentelemetry_otlp::SpanExporterBuilder, ConfigurationError> {
        match &self {
            #[cfg(feature = "otlp-grpc")]
            Exporter::Grpc(exporter) => Ok(exporter.clone().unwrap_or_default().exporter()?.into()),
            #[cfg(feature = "otlp-http")]
            Exporter::Http(exporter) => Ok(exporter.clone().unwrap_or_default().exporter()?.into()),
        }
    }

    pub fn exporter_from_env() -> Result<opentelemetry_otlp::SpanExporterBuilder, ConfigurationError>
    {
        match std::env::var("ROUTER_TRACING").as_deref() {
            #[cfg(feature = "otlp-http")]
            Ok("http") => Ok(HttpExporter::exporter_from_env().into()),
            #[cfg(feature = "otlp-grpc")]
            Ok("tonic") => Ok(GrpcExporter::exporter_from_env().into()),
            #[cfg(not(any(feature = "otlp-http", feature = "otlp-grpc")))]
            Ok(val) => Err(ConfigurationError::InvalidEnvironmentVariable(format!(
                "unrecognized value for ROUTER_TRACING: {} - this router is built without support for OpenTelemetry",
                val
            ))),
            #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
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

        pipeline = pipeline
            .with_trace_config(self.trace_config.clone().unwrap_or_default().trace_config());

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

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ExportConfig {
    #[serde(deserialize_with = "endpoint_url", default)]
    pub endpoint: Option<Url>,

    #[schemars(schema_with = "option_protocol_schema")]
    pub protocol: Option<Protocol>,
    pub timeout: Option<u64>,
}

fn option_protocol_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<ProtocolMirror>::json_schema(gen)
}

//This is a copy of the Otel protocol enum so that ExportConfig can generate json schema.
#[derive(JsonSchema)]
#[allow(dead_code)]
enum ProtocolMirror {
    /// GRPC protocol
    Grpc,
    // HttpJson,
    /// HTTP protocol with binary protobuf
    HttpBinary,
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
