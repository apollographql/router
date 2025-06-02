//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;

use http::Uri;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::TonicExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::reader::TemporalitySelector;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tonic::metadata::MetadataMap;
use tonic::transport::Certificate;
use tonic::transport::ClientTlsConfig;
use tonic::transport::Identity;
use tower::BoxError;
use url::Url;

use crate::plugins::telemetry::tracing::BatchProcessorConfig;

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable otlp
    pub(crate) enabled: bool,

    /// The endpoint to send data to
    #[serde(default)]
    pub(crate) endpoint: Option<String>,

    /// The protocol to use when sending data
    #[serde(default)]
    pub(crate) protocol: Protocol,

    /// gRPC configuration settings
    #[serde(default)]
    pub(crate) grpc: GrpcExporter,

    /// HTTP configuration settings
    #[serde(default)]
    pub(crate) http: HttpExporter,

    /// Batch processor settings
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,

    /// Temporality for export (default: `Cumulative`).
    /// Note that when exporting to Datadog agent use `Delta`.
    #[serde(default)]
    pub(crate) temporality: Temporality,
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum TelemetryDataKind {
    Traces,
    Metrics,
}

impl Config {
    pub(crate) fn exporter<T: From<HttpExporterBuilder> + From<TonicExporterBuilder>>(
        &self,
        kind: TelemetryDataKind,
    ) -> Result<T, BoxError> {
        match self.protocol {
            Protocol::Grpc => {
                // let endpoint = self.endpoint.to_full_uri(&DEFAULT_GRPC_ENDPOINT);
                let endpoint = self
                    .endpoint
                    .as_ref()
                    .map_or("", |v| if v == "default" { "" } else { v })
                    .to_string();
                // let tls_config = self.grpc.clone().to_tls_config(&endpoint)?;
                let tls_config = if endpoint != "" {
                    self.grpc
                        .clone()
                        .to_tls_config(&Uri::try_from(&endpoint).unwrap())?
                } else {
                    let tls_config_str = match kind {
                        TelemetryDataKind::Traces => format!("http://127.0.0.1/v1/traces"),
                        TelemetryDataKind::Metrics => format!("http://127.0.0.1/v1/metrics"),
                    };
                    self.grpc
                        .clone()
                        .to_tls_config(&Uri::try_from(&tls_config_str).unwrap())?
                };

                let exporter = opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_endpoint(endpoint)
                    .with_tls_config(tls_config)
                    .with_metadata(MetadataMap::from_headers(self.grpc.metadata.clone()))
                    .into();
                Ok(exporter)
            }
            Protocol::Http => {
                let endpoint = self
                    .endpoint
                    .as_ref()
                    .map_or("", |v| if v == "default" { "" } else { v })
                    .to_string();
                let http = self.http.clone();
                let exporter = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_endpoint(endpoint)
                    .with_headers(http.headers)
                    .into();
                Ok(exporter)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpExporter {
    /// Headers to send on report requests
    pub(crate) headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GrpcExporter {
    /// The optional domain name for tls config.
    /// Note that domain name is will be defaulted to match the endpoint is not explicitly set.
    pub(crate) domain_name: Option<String>,
    /// The optional certificate authority (CA) certificate to be used in TLS configuration.
    pub(crate) ca: Option<String>,
    /// The optional cert for tls config
    pub(crate) cert: Option<String>,
    /// The optional private key file for TLS configuration.
    pub(crate) key: Option<String>,

    /// gRPC metadata
    #[serde(with = "http_serde::header_map")]
    #[schemars(schema_with = "header_map", default)]
    pub(crate) metadata: http::HeaderMap,
}

fn header_map(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    HashMap::<String, Value>::json_schema(generator)
}

impl GrpcExporter {
    pub(crate) fn to_tls_config(&self, endpoint: &Uri) -> Result<ClientTlsConfig, BoxError> {
        let endpoint = endpoint
            .to_string()
            .parse::<Url>()
            .map_err(|e| BoxError::from(format!("invalid GRPC endpoint {}, {}", endpoint, e)))?;
        let domain_name = self.default_tls_domain(&endpoint);

        if let (Some(ca), Some(key), Some(cert), Some(domain_name)) =
            (&self.ca, &self.key, &self.cert, domain_name)
        {
            Ok(ClientTlsConfig::new()
                .with_native_roots()
                .domain_name(domain_name)
                .ca_certificate(Certificate::from_pem(ca.clone()))
                .identity(Identity::from_pem(cert.clone(), key.clone())))
        } else {
            // This was a breaking change in tonic where we now have to specify native roots.
            Ok(ClientTlsConfig::new().with_native_roots())
        }
    }

    fn default_tls_domain<'a>(&'a self, endpoint: &'a Url) -> Option<&'a str> {
        match (&self.domain_name, endpoint) {
            // If the URL contains the https scheme then default the tls config to use the domain from the URL. We know it's TLS.
            // If the URL contains no scheme and the port is 443 emit a warning suggesting that they may have forgotten to configure TLS domain.
            (Some(domain), _) => Some(domain.as_str()),
            (None, endpoint) if endpoint.scheme() == "https" => endpoint.host_str(),
            (None, endpoint) if endpoint.port() == Some(443) && endpoint.scheme() != "http" => {
                tracing::warn!(
                    "telemetry otlp exporter has been configured with port 443 but TLS domain has not been set. This is likely a configuration error"
                );
                None
            }
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Protocol {
    #[default]
    Grpc,
    Http,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Temporality {
    /// Export cumulative metrics.
    #[default]
    Cumulative,
    /// Export delta metrics. `Delta` should be used when exporting to DataDog Agent.
    Delta,
}

pub(crate) struct CustomTemporalitySelector(
    pub(crate) opentelemetry_sdk::metrics::data::Temporality,
);

impl TemporalitySelector for CustomTemporalitySelector {
    fn temporality(&self, _kind: InstrumentKind) -> opentelemetry_sdk::metrics::data::Temporality {
        self.0
    }
}

impl From<&Temporality> for Box<dyn TemporalitySelector> {
    fn from(value: &Temporality) -> Self {
        Box::new(match value {
            Temporality::Cumulative => {
                CustomTemporalitySelector(opentelemetry_sdk::metrics::data::Temporality::Cumulative)
            }
            Temporality::Delta => {
                CustomTemporalitySelector(opentelemetry_sdk::metrics::data::Temporality::Delta)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_grpc_defaulting_no_scheme() {
        let url = Url::parse("api.apm.com:433").unwrap();
        let exporter = GrpcExporter::default();
        let domain = exporter.default_tls_domain(&url);
        assert_eq!(domain, None);
    }

    #[test]
    fn endpoint_grpc_defaulting_scheme() {
        let url = Url::parse("https://api.apm.com:433").unwrap();
        let exporter = GrpcExporter::default();
        let domain = exporter.default_tls_domain(&url);
        assert_eq!(domain, Some(url.domain().expect("domain was expected")),);
    }

    #[test]
    fn endpoint_grpc_explicit_domain() {
        let url = Url::parse("https://api.apm.com:433").unwrap();
        let exporter = GrpcExporter {
            domain_name: Some("foo.bar".to_string()),
            ..Default::default()
        };
        let domain = exporter.default_tls_domain(&url);
        assert_eq!(domain, Some("foo.bar"));
    }
}
