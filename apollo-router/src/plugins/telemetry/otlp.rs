//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;

use indexmap::map::Entry;
use indexmap::IndexMap;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::TonicExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;
use tonic::metadata::MetadataMap;
use tonic::transport::Certificate;
use tonic::transport::ClientTlsConfig;
use tonic::transport::Identity;
use tower::BoxError;
use url::Url;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::tracing::parse_url_for_endpoint;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The endpoint to send data to
    #[serde(deserialize_with = "deser_endpoint")]
    #[schemars(with = "String")]
    pub(crate) endpoint: Endpoint,

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
        let endpoint = match (self.endpoint.clone(), &self.protocol) {
            // # https://github.com/apollographql/router/issues/2036
            // Opentelemetry rust incorrectly defaults to https
            // This will override the defaults to that of the spec
            // https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/protocol/exporter.md
            (Endpoint::Default(_), Protocol::Http) => {
                Url::parse("http://localhost:4318").expect("default url is valid")
            }
            // Default is GRPC
            (Endpoint::Default(_), Protocol::Grpc) => {
                Url::parse("http://localhost:4317").expect("default url is valid")
            }
            (Endpoint::Url(s), _) => s,
        };
        match self.protocol {
            Protocol::Grpc => {
                let grpc = self.grpc.clone();
                let exporter = opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_env()
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_endpoint(endpoint.as_str())
                    .with(&grpc.try_from(&endpoint)?, |b, t| {
                        b.with_tls_config(t.clone())
                    })
                    .with_metadata(self.grpc.metadata.clone())
                    .into();
                Ok(exporter)
            }
            Protocol::Http => {
                let http = self.http.clone();
                let exporter = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_env()
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_endpoint(endpoint.as_str())
                    .with_headers(http.headers)
                    .into();

                Ok(exporter)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum Endpoint {
    Default(EndpointDefault),
    Url(Url),
}

fn deser_endpoint<'de, D>(deserializer: D) -> Result<Endpoint, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s == "default" {
        return Ok(Endpoint::Default(EndpointDefault::Default));
    }

    let url = parse_url_for_endpoint(s).map_err(serde::de::Error::custom)?;

    Ok(Endpoint::Url(url))
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum EndpointDefault {
    Default,
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
    pub(crate) fn try_from(self, endpoint: &Url) -> Result<Option<ClientTlsConfig>, BoxError> {
        let domain_name = self.default_tls_domain(endpoint);

        if self.ca.is_some() || self.key.is_some() || self.cert.is_some() || domain_name.is_some() {
            Some(
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
            .transpose()
        } else {
            Ok(None)
        }
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
    fn endpoint_configuration() {
        let config: Config = serde_yaml::from_str("endpoint: default").unwrap();
        assert_eq!(config.endpoint, Endpoint::Default(EndpointDefault::Default));

        let config: Config = serde_yaml::from_str("endpoint: collector:1234").unwrap();
        assert_eq!(
            config.endpoint,
            Endpoint::Url(Url::parse("http://collector:1234").unwrap())
        );

        let config: Config = serde_yaml::from_str("endpoint: https://collector:1234").unwrap();
        assert_eq!(
            config.endpoint,
            Endpoint::Url(Url::parse("https://collector:1234").unwrap())
        );

        let config: Config = serde_yaml::from_str("endpoint: 127.0.0.1:1234").unwrap();
        assert_eq!(
            config.endpoint,
            Endpoint::Url(Url::parse("http://127.0.0.1:1234").unwrap())
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
