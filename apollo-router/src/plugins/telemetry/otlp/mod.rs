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
use futures::{Stream, StreamExt};
use opentelemetry::{
    sdk::metrics::{selectors, PushController},
    util::tokio_interval_stream,
};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Otlp {
    // TODO: in a future iteration we should get rid of tracing and put tracing at the root level cf https://github.com/apollographql/router/issues/683
    pub tracing: Option<Tracing>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Tracing {
    pub exporter: Exporter,
    pub trace_config: Option<TraceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Metrics {
    #[serde(flatten)]
    pub exporter: Exporter,
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

    pub fn metrics_exporter(&self) -> Result<PushController, ConfigurationError> {
        match &self {
            #[cfg(feature = "otlp-grpc")]
            Exporter::Grpc(exporter) => {
                let mut export_config = opentelemetry_otlp::ExportConfig::default();
                if let Some(grpc_exporter_cfg) = exporter {
                    if let Some(endpoint) = &grpc_exporter_cfg.export_config.endpoint {
                        export_config.endpoint = endpoint.clone().to_string();
                    }
                    if let Some(timeout) = &grpc_exporter_cfg.export_config.timeout {
                        export_config.timeout = Duration::from_secs(*timeout);
                    }
                    if let Some(protocol) = &grpc_exporter_cfg.export_config.protocol {
                        export_config.protocol = *protocol;
                    }
                }

                let push_ctrl = opentelemetry_otlp::new_pipeline()
                    .metrics(tokio::spawn, delayed_interval)
                    .with_exporter(
                        opentelemetry_otlp::new_exporter()
                            .tonic()
                            .with_export_config(export_config),
                    )
                    .with_aggregator_selector(selectors::simple::Selector::Exact)
                    .build()?;

                Ok(push_ctrl)
            }
            #[cfg(feature = "otlp-http")]
            Exporter::Http(_exporter) => {
                // let mut export_config = opentelemetry_otlp::ExportConfig::default();
                // if let Some(http_exporter_cfg) = exporter {
                //     if let Some(endpoint) = &http_exporter_cfg.export_config.endpoint {
                //         export_config.endpoint = endpoint.clone().to_string();
                //     }
                //     if let Some(timeout) = &http_exporter_cfg.export_config.timeout {
                //         export_config.timeout = Duration::from_secs(*timeout);
                //     }
                //     if let Some(protocol) = &http_exporter_cfg.export_config.protocol {
                //         export_config.protocol = *protocol;
                //     }
                // }

                // let push_ctrl = opentelemetry_otlp::new_pipeline()
                //     .metrics(tokio::spawn, delayed_interval)
                //     .with_exporter(
                //         opentelemetry_otlp::new_exporter()
                //             .http()
                //             .with_export_config(export_config),
                //     )
                //     .with_aggregator_selector(selectors::simple::Selector::Exact)
                //     .build()?;

                // Ok(push_ctrl)
                // Related to this issue https://github.com/open-telemetry/opentelemetry-rust/issues/772
                unimplemented!("cannot export metrics to http with otlp")
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ExportConfig {
    #[serde(deserialize_with = "endpoint_url", default)]
    pub endpoint: Option<Url>,

    #[schemars(schema_with = "option_protocol_schema", default)]
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

fn delayed_interval(duration: Duration) -> impl Stream<Item = tokio::time::Instant> {
    tokio_interval_stream(duration).skip(1)
}
