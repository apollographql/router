//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tower::BoxError;

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

// In older versions of `opentelemetry_otlp` the crate would "helpfully" try to make sure that the
// path for metrics or tracing was correct. This didn't always work consistently and so we added
// some code to the router to try and make this work better. We also implemented configuration so
// that:
//  - "default" would result in the default from the specification
//  - "<host>:<port>" would be an acceptable value even though no path was specified.
//
// The latter is particularly problematic, since this used to work in version 0.13, but had stopped
// working by the time we updated to 0.17.
//
// Our previous implementation didn't perform endpoint manipulation for metrics, so this
// implementation unifies the processing of endpoints.
//
// The processing does the following:
//  - If an endpoint is not specified, this results in `None`
//  - If an endpoint is specified as "default", this results in `""`
//  - If an endpoint is `""` or ends with a protocol appropriate suffix, we stop processing
//  - If we continue processing:
//      - If an endpoint has no scheme, we prepend "http://"
//      - If our endpoint has no path, we append a protocol specific suffix
//      - If it has a path, we return it unmodified
//
// Note: "" is the empty string and is thus interpreted by any opentelemetry sdk as indicating that
// the default endpoint should be used.
//
// If you are interested in learning more about opentelemetry endpoints:
//  https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/protocol/exporter.md
// contains the details.
pub(super) fn process_endpoint(
    endpoint: &Option<String>,
    kind: &TelemetryDataKind,
    protocol: &Protocol,
) -> Result<Option<String>, BoxError> {
    // If there is no endpoint, None, do no processing because the user must be relying on the
    // router processing OTEL environment variables for endpoint.
    // If there is an endpoint, Some(value), we must process that value. Most of this processing is
    // performed to try and remain backwards compatible with previous versions of the router which
    // depended on "non-standard" behaviour of the opentelemetry_otlp crate. I've tried documenting
    // each of the outcomes clearly for the benefit of future maintainers.
    endpoint
        .as_ref()
        .map(|v| {
            let mut base = if v == "default" {
                "".to_string()
            } else {
                v.to_string()
            };
            if base.is_empty() {
                // We don't want to process empty strings
                Ok(base)
            } else {
                // We require a scheme on our endpoint or we can't parse it as a Uri.
                // If we don't have one, prepend with "http://"
                if !base.starts_with("http") {
                    base = format!("http://{base}");
                }
                // We expect different suffixes by protocol and signal type
                let suffix = match protocol {
                    Protocol::Grpc => "/",
                    Protocol::Http => match kind {
                        TelemetryDataKind::Metrics => "/v1/metrics",
                        TelemetryDataKind::Traces => "/v1/traces",
                    },
                };
                if base.ends_with(suffix) {
                    // Our suffix is in place, all is good
                    Ok(base)
                } else {
                    let uri = http::Uri::try_from(&base)?;
                    // Note: If our endpoint is "<scheme>:://host:port", then the path will be "/".
                    // We already ensured that our base does not end with <suffix>, so we must append
                    // <suffix>
                    if uri.path() == "/" {
                        // Remove any trailing slash from the base so we don't end up with a
                        // double slash when concatenating e.g. "http://my-base//v1/metrics"
                        if base.ends_with("/") {
                            base.pop();
                        }
                        // We don't have a path, we need to add one
                        Ok(format!("{base}{suffix}"))
                    } else {
                        // We have a path, it doesn't end with <suffix>, let it pass...
                        // We could try and enforce the standard here and only let through paths
                        // which end with the expected suffix. However, I think that would reduce
                        // backwards compatibility and we should just trust that the user knows
                        // what they are doing.
                        Ok(base)
                    }
                }
            }
        })
        .transpose()
}

impl Config {
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

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::metrics::data::Temporality as SdkTemporality;

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

    #[test]
    fn test_process_endpoint() {
        // Traces
        let endpoint = None;
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("default".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(Some("".to_string()), processed_endpoint);

        let endpoint = Some("https://api.apm.com:433/v1/traces".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("https://api.apm.com:433".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(
            Some("https://api.apm.com:433/".to_string()),
            processed_endpoint
        );

        let endpoint = Some("https://api.apm.com:433".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Http).unwrap();
        assert_eq!(
            Some("https://api.apm.com:433/v1/traces".to_string()),
            processed_endpoint
        );

        let endpoint = Some("https://api.apm.com:433/".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("https://api.apm.com:433/traces".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("localhost:4317".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Grpc).unwrap();
        assert_eq!(
            Some("http://localhost:4317/".to_string()),
            processed_endpoint
        );

        let endpoint = Some("localhost:4317".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Http).unwrap();
        assert_eq!(
            Some("http://localhost:4317/v1/traces".to_string()),
            processed_endpoint
        );

        let endpoint = Some("https://otlp.nr-data.net".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Traces, &Protocol::Http).unwrap();
        assert_eq!(
            Some("https://otlp.nr-data.net/v1/traces".to_string()),
            processed_endpoint
        );

        // Metrics
        let endpoint = None;
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(None, processed_endpoint);

        let endpoint = Some("default".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(Some("".to_string()), processed_endpoint);

        let endpoint = Some("https://api.apm.com:433/v1/metrics".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("https://api.apm.com:433".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(
            Some("https://api.apm.com:433/".to_string()),
            processed_endpoint
        );

        let endpoint = Some("https://api.apm.com:433".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Http).unwrap();
        assert_eq!(
            Some("https://api.apm.com:433/v1/metrics".to_string()),
            processed_endpoint
        );

        let endpoint = Some("https://api.apm.com:433/".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("https://api.apm.com:433/metrics".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(endpoint, processed_endpoint);

        let endpoint = Some("localhost:4317".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Grpc).unwrap();
        assert_eq!(
            Some("http://localhost:4317/".to_string()),
            processed_endpoint
        );

        let endpoint = Some("localhost:4317".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Http).unwrap();
        assert_eq!(
            Some("http://localhost:4317/v1/metrics".to_string()),
            processed_endpoint
        );

        let endpoint = Some("https://otlp.nr-data.net".to_string());
        let processed_endpoint =
            process_endpoint(&endpoint, &TelemetryDataKind::Metrics, &Protocol::Http).unwrap();
        assert_eq!(
            Some("https://otlp.nr-data.net/v1/metrics".to_string()),
            processed_endpoint
        );
    }
}
