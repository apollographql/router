use crate::configuration::ConfigurationError;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::{GenericWith, TracingConfigurator};
#[cfg(feature = "otlp-grpc")]
use grpc::*;
use opentelemetry::sdk::trace::Builder;
use opentelemetry_otlp::{SpanExporterBuilder, WithExportConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::result::Result;
use std::time::Duration;
use tower::BoxError;
use url::Url;

#[cfg(feature = "otlp-http")]
use self::http::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint: Endpoint,
    pub protocol: Option<Protocol>,

    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub timeout: Option<Duration>,
    #[cfg(feature = "otlp-grpc")]
    pub grpc: Option<GrpcExporter>,
    #[cfg(feature = "otlp-http")]
    pub http: Option<HttpExporter>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub enum Endpoint {
    Default(EndpointDefault),
    Url(Url),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub enum EndpointDefault {
    Default,
}

#[cfg(feature = "otlp-http")]
mod http {
    use super::*;
    #[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct HttpExporter {
        pub headers: Option<HashMap<String, String>>,
    }
}

#[cfg(feature = "otlp-grpc")]
mod grpc {
    use super::*;
    use serde_json::Value;
    use tonic::metadata::MetadataMap;
    use tonic::transport::ClientTlsConfig;

    #[cfg(feature = "otlp-grpc")]
    #[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct GrpcExporter {
        #[serde(flatten)]
        pub tls_config: Option<TlsConfig>,
        #[serde(
            deserialize_with = "header_map_serde::deserialize",
            serialize_with = "header_map_serde::serialize",
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

    mod header_map_serde {
        use super::*;
        use tonic::metadata::{KeyAndValueRef, MetadataKey};

        pub(crate) fn serialize<S>(
            map: &Option<MetadataMap>,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
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
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Secret {
    Env(String),
    File(PathBuf),
}

impl Secret {
    pub fn read(&self) -> Result<String, ConfigurationError> {
        match self {
            Secret::Env(s) => std::env::var(s).map_err(ConfigurationError::CannotReadSecretFromEnv),
            Secret::File(path) => {
                std::fs::read_to_string(path).map_err(ConfigurationError::CannotReadSecretFromFile)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub enum Protocol {
    #[cfg(feature = "otlp-grpc")]
    Grpc,
    #[cfg(feature = "otlp-http")]
    Http,
}

impl Default for Protocol {
    fn default() -> Self {
        vec![
            #[cfg(feature = "otlp-grpc")]
            Protocol::Grpc,
            #[cfg(feature = "otlp-http")]
            Protocol::Http,
        ]
        .get(0)
        .expect("at least one feature must be present for otel, qed")
        .clone()
    }
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, _trace_config: &Trace) -> Result<Builder, BoxError> {
        let endpoint = match &self.endpoint {
            Endpoint::Default(_) => None,
            Endpoint::Url(s) => Some(s),
        };
        match self.protocol.clone().unwrap_or_default() {
            #[cfg(feature = "otlp-grpc")]
            Protocol::Grpc => {
                let grpc = self.grpc.clone().unwrap_or_default();
                let exporter: SpanExporterBuilder = opentelemetry_otlp::new_exporter()
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
                Ok(builder.with_batch_exporter(
                    exporter.build_span_exporter()?,
                    opentelemetry::runtime::Tokio,
                ))
            }
            #[cfg(feature = "otlp-http")]
            Protocol::Http => {
                let http = self.http.clone().unwrap_or_default();
                let exporter: SpanExporterBuilder = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_env()
                    .with(&self.timeout, |b, t| b.with_timeout(*t))
                    .with(&endpoint, |b, e| b.with_endpoint(e.as_str()))
                    .with(&http.headers, |b, h| b.with_headers(h.clone()))
                    .into();
                Ok(builder.with_batch_exporter(
                    exporter.build_span_exporter()?,
                    opentelemetry::runtime::Tokio,
                ))
            }
        }
    }
}

#[cfg(test)]
#[cfg(feature = "otlp-http")]
mod tests {
    use crate::plugins::telemetry::tracing::test::run_query;
    use opentelemetry::global;
    use opentelemetry::sdk::propagation::TraceContextPropagator;
    use tower::BoxError;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    // This test can be run manually from your IDE to help with testing otel
    // It is set to ignore by default as otlp may not be set up
    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing() -> Result<(), BoxError> {
        tracing_subscriber::fmt().init();

        global::set_text_map_propagator(TraceContextPropagator::new());
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(opentelemetry_otlp::new_exporter().http())
            .install_batch(opentelemetry::runtime::Tokio)?;

        // Create a tracing layer with the configured tracer
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);

        // Trace executed code
        run_query().with_subscriber(subscriber).await;
        global::shutdown_tracer_provider();

        Ok(())
    }
}
