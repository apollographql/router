//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;
use std::time::Duration;

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
use tonic::transport::ClientTlsConfig;
use tower::BoxError;
use url::Url;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::tracing::parse_url_for_endpoint;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(deserialize_with = "deser_endpoint")]
    #[schemars(with = "String")]
    pub(crate) endpoint: Endpoint,
    pub(crate) protocol: Option<Protocol>,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) timeout: Option<Duration>,
    pub(crate) grpc: Option<GrpcExporter>,
    pub(crate) http: Option<HttpExporter>,
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
            (Endpoint::Default(_), Some(Protocol::Http)) => {
                Some(Url::parse("http://localhost:4318").expect("default url is valid"))
            }
            // Default is GRPC
            (Endpoint::Default(_), _) => {
                Some(Url::parse("http://localhost:4317").expect("default url is valid"))
            }
            (Endpoint::Url(s), _) => Some(s),
        };
        match self.protocol.clone().unwrap_or_default() {
            Protocol::Grpc => {
                let grpc = self.grpc.clone().unwrap_or_default();
                let exporter = opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_env()
                    .with(&self.timeout, |b, t| b.with_timeout(*t))
                    .with(&endpoint, |b, e| b.with_endpoint(e.as_str()))
                    .try_with(&grpc.tls_config.defaulted(endpoint.as_ref()), |b, t| {
                        Ok(b.with_tls_config(t.try_into()?))
                    })?
                    .with(&grpc.metadata, |b, m| b.with_metadata(m.clone()))
                    .into();
                Ok(exporter)
            }
            Protocol::Http => {
                let http = self.http.clone().unwrap_or_default();
                let exporter = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_env()
                    .with(&self.timeout, |b, t| b.with_timeout(*t))
                    .with(&endpoint, |b, e| b.with_endpoint(e.as_str()))
                    .with(&http.headers, |b, h| b.with_headers(h.clone()))
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
#[serde(deny_unknown_fields)]
pub(crate) struct HttpExporter {
    pub(crate) headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrpcExporter {
    #[serde(flatten)]
    pub(crate) tls_config: TlsConfig,
    #[serde(
        deserialize_with = "metadata_map_serde::deserialize",
        serialize_with = "metadata_map_serde::serialize",
        default
    )]
    #[schemars(schema_with = "option_metadata_map", default)]
    pub(crate) metadata: Option<MetadataMap>,
}

fn option_metadata_map(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<HashMap<String, Value>>::json_schema(gen)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsConfig {
    domain_name: Option<String>,
    ca: Option<String>,
    cert: Option<String>,
    key: Option<String>,
}

impl TlsConfig {
    // Return a TlsConfig if it has something actually set.
    pub(crate) fn defaulted(mut self, endpoint: Option<&Url>) -> Option<TlsConfig> {
        if let Some(endpoint) = endpoint {
            if self.domain_name.is_none() {
                // If the URL contains the https scheme then default the tls config to use the domain from the URL. We know it's TLS.
                // If the URL contains no scheme and the port is 443 emit a warning suggesting that they may have forgotten to configure TLS domain.
                if endpoint.scheme() == "https" {
                    self.domain_name = endpoint.host_str().map(|s| s.to_string())
                } else if endpoint.port() == Some(443) && endpoint.scheme() != "http" {
                    tracing::warn!("telemetry otlp exporter has been configured with port 443 but TLS domain has not been set. This is likely a configuration error")
                }
            }
        }

        if self.ca.is_some()
            || self.key.is_some()
            || self.cert.is_some()
            || self.domain_name.is_some()
        {
            Some(self)
        } else {
            None
        }
    }
}

impl TryFrom<&TlsConfig> for tonic::transport::channel::ClientTlsConfig {
    type Error = BoxError;

    fn try_from(config: &TlsConfig) -> Result<ClientTlsConfig, Self::Error> {
        ClientTlsConfig::new()
            .with(&config.domain_name, |b, d| b.domain_name(d))
            .try_with(&config.ca, |b, c| {
                Ok(b.ca_certificate(tonic::transport::Certificate::from_pem(c)))
            })?
            .try_with(
                &config.cert.clone().zip(config.key.clone()),
                |b, (cert, key)| Ok(b.identity(tonic::transport::Identity::from_pem(cert, key))),
            )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Protocol {
    Grpc,
    Http,
}

impl Default for Protocol {
    fn default() -> Self {
        Protocol::Grpc
    }
}

mod metadata_map_serde {
    use tonic::metadata::KeyAndValueRef;
    use tonic::metadata::MetadataKey;

    use super::*;

    pub(crate) fn serialize<S>(map: &Option<MetadataMap>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        if map.as_ref().map(|x| x.is_empty()).unwrap_or(true) {
            return serializer.serialize_none();
        }

        let mut serializable_format: IndexMap<&str, Vec<&str>> = IndexMap::new();

        for key_and_value in map.iter().flat_map(|x| x.iter()) {
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

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<MetadataMap>, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let serializable_format: IndexMap<String, Vec<String>> =
            Deserialize::deserialize(deserializer)?;

        if serializable_format.is_empty() {
            return Ok(None);
        }

        let mut map = MetadataMap::new();

        for (key, values) in serializable_format.into_iter() {
            let key = MetadataKey::from_bytes(key.as_bytes()).unwrap();
            for value in values {
                map.append(key.clone(), value.parse().unwrap());
            }
        }

        Ok(Some(map))
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
            serialize(&Some(map), &mut ser).unwrap();
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
        let tls = TlsConfig::default().defaulted(Some(&url));
        assert_eq!(tls, None);
    }

    #[test]
    fn default_endpoint() {
        let tls = TlsConfig::default().defaulted(None);
        assert_eq!(tls, None);
    }

    #[test]
    fn endpoint_grpc_defaulting_scheme() {
        let url = Url::parse("https://api.apm.com:433").unwrap();
        let tls = TlsConfig::default().defaulted(Some(&url));
        assert_eq!(
            tls,
            Some(TlsConfig {
                domain_name: url.domain().map(|d| d.to_string()),
                ca: None,
                cert: None,
                key: None
            })
        );
    }

    #[test]
    fn endpoint_grpc_defaulting_no_endpoint() {
        let tls = TlsConfig::default().defaulted(None);
        assert_eq!(tls, None);
    }

    #[test]
    fn endpoint_grpc_explicit_domain() {
        let url = Url::parse("https://api.apm.com:433").unwrap();
        let tls = TlsConfig {
            domain_name: Some("foo.bar".to_string()),
            ..Default::default()
        }
        .defaulted(Some(&url));
        assert_eq!(
            tls,
            Some(TlsConfig {
                domain_name: Some("foo.bar".to_string()),
                ca: None,
                cert: None,
                key: None
            })
        );
    }
}
