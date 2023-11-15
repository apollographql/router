//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;

use http::Uri;
use indexmap::map::Entry;
use indexmap::IndexMap;
use lazy_static::lazy_static;
use opentelemetry::sdk::metrics::reader::TemporalitySelector;
use opentelemetry::sdk::metrics::InstrumentKind;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::TonicExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
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

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

lazy_static! {
    static ref DEFAULT_GRPC_ENDPOINT: Uri = Uri::from_static("http://127.0.0.1:4317");
    static ref DEFAULT_HTTP_ENDPOINT: Uri = Uri::from_static("http://127.0.0.1:4318");
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable otlp
    pub(crate) enabled: bool,

    /// The endpoint to send data to
    #[serde(default)]
    pub(crate) endpoint: UriEndpoint,

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

impl Config {
    pub(crate) fn exporter<T: From<HttpExporterBuilder> + From<TonicExporterBuilder>>(
        &self,
    ) -> Result<T, BoxError> {
        match self.protocol {
            Protocol::Grpc => {
                let endpoint = self.endpoint.to_uri(&DEFAULT_GRPC_ENDPOINT);
                let grpc = self.grpc.clone();
                let exporter = opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_env()
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with(&endpoint, |b, endpoint| {
                        b.with_endpoint(endpoint.to_string())
                    })
                    .with(&grpc.try_from(&endpoint)?, |b, t| {
                        b.with_tls_config(t.clone())
                    })
                    .with_metadata(self.grpc.metadata.clone())
                    .into();
                Ok(exporter)
            }
            Protocol::Http => {
                let endpoint = self.endpoint.to_uri(&DEFAULT_HTTP_ENDPOINT);
                let http = self.http.clone();
                let exporter = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_env()
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with(&endpoint, |b, endpoint| {
                        b.with_endpoint(endpoint.to_string())
                    })
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
    #[serde(
        deserialize_with = "metadata_map_serde::deserialize",
        serialize_with = "metadata_map_serde::serialize"
    )]
    #[schemars(schema_with = "header_map", default)]
    pub(crate) metadata: MetadataMap,
}

fn header_map(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    HashMap::<String, Value>::json_schema(gen)
}

impl GrpcExporter {
    // Return a TlsConfig if it has something actually set.
    pub(crate) fn try_from(
        self,
        endpoint: &Option<Uri>,
    ) -> Result<Option<ClientTlsConfig>, BoxError> {
        if let Some(endpoint) = endpoint {
            let endpoint = endpoint.to_string().parse::<Url>().map_err(|e| {
                BoxError::from(format!("invalid GRPC endpoint {}, {}", endpoint, e))
            })?;
            let domain_name = self.default_tls_domain(&endpoint);

            if self.ca.is_some()
                || self.key.is_some()
                || self.cert.is_some()
                || domain_name.is_some()
            {
                return Some(
                    ClientTlsConfig::new()
                        .with(&domain_name, |b, d| b.domain_name(*d))
                        .try_with(&self.ca, |b, c| {
                            Ok(b.ca_certificate(Certificate::from_pem(c)))
                        })?
                        .try_with(
                            &self.cert.clone().zip(self.key.clone()),
                            |b, (cert, key)| Ok(b.identity(Identity::from_pem(cert, key))),
                        ),
                )
                .transpose();
            }
        }
        Ok(None)
    }

    fn default_tls_domain<'a>(&'a self, endpoint: &'a Url) -> Option<&'a str> {
        let domain_name = match (&self.domain_name, endpoint) {
            // If the URL contains the https scheme then default the tls config to use the domain from the URL. We know it's TLS.
            // If the URL contains no scheme and the port is 443 emit a warning suggesting that they may have forgotten to configure TLS domain.
            (Some(domain), _) => Some(domain.as_str()),
            (None, endpoint) if endpoint.scheme() == "https" => endpoint.host_str(),
            (None, endpoint) if endpoint.port() == Some(443) && endpoint.scheme() != "http" => {
                tracing::warn!("telemetry otlp exporter has been configured with port 443 but TLS domain has not been set. This is likely a configuration error");
                None
            }
            _ => None,
        };
        domain_name
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
    pub(crate) opentelemetry::sdk::metrics::data::Temporality,
);

impl TemporalitySelector for CustomTemporalitySelector {
    fn temporality(&self, _kind: InstrumentKind) -> opentelemetry::sdk::metrics::data::Temporality {
        self.0
    }
}

impl From<&Temporality> for Box<dyn TemporalitySelector> {
    fn from(value: &Temporality) -> Self {
        Box::new(match value {
            Temporality::Cumulative => CustomTemporalitySelector(
                opentelemetry::sdk::metrics::data::Temporality::Cumulative,
            ),
            Temporality::Delta => {
                CustomTemporalitySelector(opentelemetry::sdk::metrics::data::Temporality::Delta)
            }
        })
    }
}

mod metadata_map_serde {
    use tonic::metadata::KeyAndValueRef;
    use tonic::metadata::MetadataKey;

    use super::*;

    pub(crate) fn serialize<S>(map: &MetadataMap, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let mut serializable_format: IndexMap<&str, Vec<&str>> = IndexMap::new();

        for key_and_value in map.iter() {
            match key_and_value {
                KeyAndValueRef::Ascii(key, value) => {
                    match serializable_format.entry(key.as_str()) {
                        Entry::Vacant(values) => {
                            values.insert(vec![value.to_str().unwrap()]);
                        }
                        Entry::Occupied(mut values) => {
                            values.get_mut().push(value.to_str().unwrap())
                        }
                    }
                }
                KeyAndValueRef::Binary(_, _) => todo!(),
            };
        }

        serializable_format.serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<MetadataMap, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let serializable_format: IndexMap<String, Vec<String>> =
            Deserialize::deserialize(deserializer)?;

        let mut map = MetadataMap::new();

        for (key, values) in serializable_format.into_iter() {
            let key = MetadataKey::from_bytes(key.as_bytes()).unwrap();
            for value in values {
                map.append(key.clone(), value.parse().unwrap());
            }
        }

        Ok(map)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn serialize_metadata_map() {
            let mut map = MetadataMap::new();
            map.append("foo", "bar".parse().unwrap());
            map.append("foo", "baz".parse().unwrap());
            map.append("bar", "foo".parse().unwrap());
            let mut buffer = Vec::new();
            let mut ser = serde_yaml::Serializer::new(&mut buffer);
            serialize(&map, &mut ser).unwrap();
            insta::assert_snapshot!(std::str::from_utf8(&buffer).unwrap());
            let de = serde_yaml::Deserializer::from_slice(&buffer);
            deserialize(de).unwrap();
        }
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
