//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_otlp::tonic_types::transport::ClientTlsConfig;
use opentelemetry_otlp::tonic_types::transport::Certificate;
use opentelemetry_otlp::tonic_types::transport::Identity;
use tower::BoxError;
use url::Url;
use http::Uri;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable otlp
    pub(crate) enabled: bool,

    /// Batch processor settings
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum TelemetryDataKind {
    Traces,
    Metrics,
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
            .map_err(|e| BoxError::from(format!("invalid GRPC endpoint {endpoint}, {e}")))?;
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
    pub(crate) opentelemetry_sdk::metrics::Temporality,
);

impl CustomTemporalitySelector {
    fn temporality(&self, kind: InstrumentKind) -> opentelemetry_sdk::metrics::Temporality {
        // Up/down counters should always use cumulative temporality to ensure they are sent as aggregates
        // rather than deltas, which prevents drift issues.
        // See https://github.com/open-telemetry/opentelemetry-specification/blob/a1c13d59bb7d0fb086df2b3e1eaec9df9efef6cc/specification/metrics/sdk_exporters/otlp.md#additional-configuration for mor information
        match kind {
            InstrumentKind::UpDownCounter | InstrumentKind::ObservableUpDownCounter => {
                opentelemetry_sdk::metrics::Temporality::Cumulative
            }
            _ => self.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::metrics::data::Temporality as SdkTemporality;

    use super::*;
    use url::Url;

    #[test]
    fn test_updown_counter_temporality_override() {
        // Test that up/down counters always get cumulative temporality regardless of configuration
        let delta_selector = CustomTemporalitySelector(SdkTemporality::Delta);
        let cumulative_selector = CustomTemporalitySelector(SdkTemporality::Cumulative);

        // UpDownCounter should always be cumulative
        assert_eq!(
            delta_selector.temporality(InstrumentKind::UpDownCounter),
            SdkTemporality::Cumulative,
            "UpDownCounter should always use cumulative temporality even with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::UpDownCounter),
            SdkTemporality::Cumulative,
            "UpDownCounter should use cumulative temporality with cumulative config"
        );

        // ObservableUpDownCounter should always be cumulative
        assert_eq!(
            delta_selector.temporality(InstrumentKind::ObservableUpDownCounter),
            SdkTemporality::Cumulative,
            "ObservableUpDownCounter should always use cumulative temporality even with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::ObservableUpDownCounter),
            SdkTemporality::Cumulative,
            "ObservableUpDownCounter should use cumulative temporality with cumulative config"
        );
    }

    #[test]
    fn test_counter_temporality_respects_config() {
        // Test that regular counters respect the configured temporality
        let delta_selector = CustomTemporalitySelector(SdkTemporality::Delta);
        let cumulative_selector = CustomTemporalitySelector(SdkTemporality::Cumulative);

        // Counter should respect configuration
        assert_eq!(
            delta_selector.temporality(InstrumentKind::Counter),
            SdkTemporality::Delta,
            "Counter should use delta temporality with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::Counter),
            SdkTemporality::Cumulative,
            "Counter should use cumulative temporality with cumulative config"
        );

        // ObservableCounter should respect configuration
        assert_eq!(
            delta_selector.temporality(InstrumentKind::ObservableCounter),
            SdkTemporality::Delta,
            "ObservableCounter should use delta temporality with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::ObservableCounter),
            SdkTemporality::Cumulative,
            "ObservableCounter should use cumulative temporality with cumulative config"
        );
    }

    #[test]
    fn test_gauge_temporality_respects_config() {
        // Test that gauges respect the configured temporality (gauges are not forced to cumulative)
        let delta_selector = CustomTemporalitySelector(SdkTemporality::Delta);
        let cumulative_selector = CustomTemporalitySelector(SdkTemporality::Cumulative);

        // Gauge should respect configuration
        assert_eq!(
            delta_selector.temporality(InstrumentKind::Gauge),
            SdkTemporality::Delta,
            "Gauge should use delta temporality with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::Gauge),
            SdkTemporality::Cumulative,
            "Gauge should use cumulative temporality with cumulative config"
        );

        // ObservableGauge should respect configuration
        assert_eq!(
            delta_selector.temporality(InstrumentKind::ObservableGauge),
            SdkTemporality::Delta,
            "ObservableGauge should use delta temporality with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::ObservableGauge),
            SdkTemporality::Cumulative,
            "ObservableGauge should use cumulative temporality with cumulative config"
        );
    }

    #[test]
    fn test_histogram_temporality_respects_config() {
        // Test that histograms respect the configured temporality
        let delta_selector = CustomTemporalitySelector(SdkTemporality::Delta);
        let cumulative_selector = CustomTemporalitySelector(SdkTemporality::Cumulative);

        // Histogram should respect configuration
        assert_eq!(
            delta_selector.temporality(InstrumentKind::Histogram),
            SdkTemporality::Delta,
            "Histogram should use delta temporality with delta config"
        );
        assert_eq!(
            cumulative_selector.temporality(InstrumentKind::Histogram),
            SdkTemporality::Cumulative,
            "Histogram should use cumulative temporality with cumulative config"
        );
    }

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
