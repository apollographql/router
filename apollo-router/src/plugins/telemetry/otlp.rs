//! Shared configuration for Otlp tracing and metrics.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

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

use crate::configuration::ConfigurationError;
use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::tracing::parse_url_for_endpoint;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(deserialize_with = "deser_endpoint")]
    #[schemars(with = "String")]
    pub endpoint: Endpoint,
    pub protocol: Option<Protocol>,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub timeout: Option<Duration>,
    pub grpc: Option<GrpcExporter>,
    pub http: Option<HttpExporter>,
}

impl Config {
    pub fn exporter<T: From<HttpExporterBuilder> + From<TonicExporterBuilder>>(
        &self,
    ) -> Result<T, BoxError> {
        let endpoint = match &self.endpoint {
            Endpoint::Default(_) => None,
            Endpoint::Url(s) => Some(s),
        };
        match self.protocol.clone().unwrap_or_default() {
            Protocol::Grpc => {
                let grpc = self.grpc.clone().unwrap_or_default();
                let exporter = opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_env()
                    .with(&self.timeout, |b, t| b.with_timeout(*t))
                    .with(&endpoint, |b, e| b.with_endpoint(e.as_str()))
                    .try_with(
                        &grpc.tls_config,
                        |b, t| Ok(b.with_tls_config(t.try_into()?)),
                    )?
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
pub enum Endpoint {
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
pub enum EndpointDefault {
    Default,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpExporter {
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrpcExporter {
    #[serde(flatten)]
    pub tls_config: Option<TlsConfig>,
    #[serde(
        deserialize_with = "metadata_map_serde::deserialize",
        serialize_with = "metadata_map_serde::serialize",
        default
    )]
    #[schemars(schema_with = "option_metadata_map", default)]
    pub metadata: Option<MetadataMap>,
}

fn option_metadata_map(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<HashMap<String, Value>>::json_schema(gen)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    domain_name: Option<String>,
    ca: Option<Secret>,
    cert: Option<Secret>,
    key: Option<Secret>,
}

impl TryFrom<&TlsConfig> for tonic::transport::channel::ClientTlsConfig {
    type Error = BoxError;

    fn try_from(config: &TlsConfig) -> Result<ClientTlsConfig, Self::Error> {
        ClientTlsConfig::new()
            .with(&config.domain_name, |b, d| b.domain_name(d))
            .try_with(&config.ca, |b, c| {
                Ok(b.ca_certificate(tonic::transport::Certificate::from_pem(c.read()?)))
            })?
            .try_with(
                &config.cert.clone().zip(config.key.clone()),
                |b, (cert, key)| {
                    Ok(b.identity(tonic::transport::Identity::from_pem(
                        cert.read()?,
                        key.read()?,
                    )))
                },
            )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Secret {
    Env(String),
    File(PathBuf),
}

impl Secret {
    pub(crate) fn read(&self) -> Result<String, ConfigurationError> {
        match self {
            Secret::Env(s) => std::env::var(s).map_err(ConfigurationError::CannotReadSecretFromEnv),
            Secret::File(path) => {
                std::fs::read_to_string(path).map_err(ConfigurationError::CannotReadSecretFromFile)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Protocol {
    Grpc,
    Http,
}

impl Default for Protocol {
    fn default() -> Self {
        Protocol::Grpc
    }
}

mod metadata_map_serde {
    use std::collections::HashMap;

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

        let mut serializable_format =
            Vec::with_capacity(map.as_ref().map(|x| x.len()).unwrap_or(0));

        serializable_format.extend(map.iter().flat_map(|x| x.iter()).map(|key_and_value| {
            match key_and_value {
                KeyAndValueRef::Ascii(key, value) => {
                    let mut map = HashMap::with_capacity(1);
                    map.insert(key.as_str(), value.to_str().unwrap());
                    map
                }
                KeyAndValueRef::Binary(_, _) => todo!(),
            }
        }));

        serializable_format.serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<MetadataMap>, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let serializable_format: Vec<HashMap<String, String>> =
            Deserialize::deserialize(deserializer)?;

        if serializable_format.is_empty() {
            return Ok(None);
        }

        let mut map = MetadataMap::new();

        for submap in serializable_format.into_iter() {
            for (key, value) in submap.into_iter() {
                let key = MetadataKey::from_bytes(key.as_bytes()).unwrap();
                map.append(key, value.parse().unwrap());
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
        assert_eq!(Endpoint::Default(EndpointDefault::Default), config.endpoint);

        let config: Config = serde_yaml::from_str("endpoint: collector:1234").unwrap();
        assert_eq!(
            Endpoint::Url(Url::parse("http://collector:1234").unwrap()),
            config.endpoint
        );

        let config: Config = serde_yaml::from_str("endpoint: https://collector:1234").unwrap();
        assert_eq!(
            Endpoint::Url(Url::parse("https://collector:1234").unwrap()),
            config.endpoint
        );

        let config: Config = serde_yaml::from_str("endpoint: 127.0.0.1:1234").unwrap();
        assert_eq!(
            Endpoint::Url(Url::parse("http://127.0.0.1:1234").unwrap()),
            config.endpoint
        );
    }
}
