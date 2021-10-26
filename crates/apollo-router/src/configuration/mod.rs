//! Logic for loading configuration in to an object model

#[cfg(any(
    all(
        feature = "otlp-tonic",
        any(feature = "otlp-grpcio", feature = "otlp-http")
    ),
    all(feature = "otlp-grpcio", feature = "otlp-http")
))]
compile_error!("you can select only one feature otlp-*!");

#[cfg(any(feature = "otlp-tonic", feature = "otlp-grpcio", feature = "otlp-http"))]
pub mod otlp;

use apollo_router_core::Schema;
use derivative::Derivative;
use displaydoc::Display;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use typed_builder::TypedBuilder;
use url::Url;

/// Configuration error.
#[derive(Debug, Error, Display)]
pub enum ConfigurationError {
    /// Could not read secret from file: {0}
    CannotReadSecretFromFile(std::io::Error),
    /// Could not read secret from environment variable: {0}
    CannotReadSecretFromEnv(std::env::VarError),
    /// Could not setup OTLP tracing: {0}
    OtlpTracing(opentelemetry::trace::TraceError),
    /// The configuration could not be loaded because it requires the feature {0:?}
    MissingFeature(&'static str),
    /// Could not find an URL for subgraph {0}
    MissingSubgraphUrl(String),
}

/// The configuration for the router.
/// Currently maintains a mapping of subgraphs.
#[derive(Clone, Derivative, Deserialize, Serialize, TypedBuilder)]
#[derivative(Debug)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    /// Configuration options pertaining to the http server component.
    #[serde(default)]
    #[builder(default)]
    pub server: Server,

    /// Mapping of name to subgraph that the router may contact.
    #[serde(default)]
    #[builder(default)]
    pub subgraphs: HashMap<String, Subgraph>,

    /// OpenTelemetry configuration.
    #[builder(default)]
    pub opentelemetry: Option<OpenTelemetry>,

    #[serde(skip)]
    #[builder(default)]
    #[derivative(Debug = "ignore")]
    pub subscriber: Option<Arc<dyn tracing::Subscriber + Send + Sync + 'static>>,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from_str("127.0.0.1:4000").unwrap()
}

impl Configuration {
    pub fn load_subgraphs(&mut self, schema: &Schema) -> Result<(), Vec<ConfigurationError>> {
        let mut errors = Vec::new();

        for (name, url) in schema.subgraphs() {
            match self.subgraphs.get(name) {
                None => {
                    if url.is_empty() {
                        errors.push(ConfigurationError::MissingSubgraphUrl(name.to_owned()));
                        continue;
                    }
                    self.subgraphs.insert(
                        name.to_owned(),
                        Subgraph {
                            routing_url: url.to_owned(),
                        },
                    );
                }
                Some(subgraph) => {
                    if !url.is_empty() && url != &subgraph.routing_url {
                        tracing::warn!("overriding URL from subgraph {} at {} with URL from the configuration file: {}",
                name, url, subgraph.routing_url);
                    }

                    if !url.is_empty() {
                        self.subgraphs.insert(
                            name.to_owned(),
                            Subgraph {
                                routing_url: url.to_owned(),
                            },
                        );
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Configuration for a subgraph.
#[derive(Debug, Clone, Deserialize, Serialize, TypedBuilder)]
pub struct Subgraph {
    /// The url for the subgraph.
    pub routing_url: String,
}

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:4000
    #[serde(default = "default_listen")]
    #[builder(default_code = "default_listen()")]
    pub listen: SocketAddr,

    /// Cross origin request headers.
    #[serde(default)]
    #[builder(default)]
    pub cors: Option<Cors>,
}

/// Cross origin request configuration.
#[derive(Debug, Clone, Deserialize, Serialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct Cors {
    #[serde(default)]
    #[builder(default)]
    /// Set to false to disallow any origin and rely exclusively on `origins`.
    ///
    /// /!\ Defaults to true
    /// Having this set to true is the only way to allow Origin: null.
    pub allow_any_origin: Option<bool>,

    /// Set to true to add the `Access-Control-Allow-Credentials` header.
    #[serde(default)]
    #[builder(default)]
    pub allow_credentials: Option<bool>,

    /// The headers to allow.
    /// Defaults to the required request header for Apollo Studio
    #[serde(default = "default_cors_headers")]
    #[builder(default_code = "default_cors_headers()")]
    pub allow_headers: Vec<String>,

    #[serde(default)]
    #[builder(default)]
    /// Which response headers should be made available to scripts running in the browser,
    /// in response to a cross-origin request.
    pub expose_headers: Option<Vec<String>>,

    /// The origin(s) to allow requests from.
    /// Use `https://studio.apollographql.com/` to allow Apollo Studio to function.
    #[serde(default)]
    #[builder(default)]
    pub origins: Vec<String>,

    /// Allowed request methods. Defaults to GET, POST, OPTIONS.
    #[serde(default = "default_cors_methods")]
    #[builder(default_code = "default_cors_methods()")]
    pub methods: Vec<String>,
}

fn default_cors_headers() -> Vec<String> {
    vec!["Content-Type".into()]
}

fn default_cors_methods() -> Vec<String> {
    vec!["GET".into(), "POST".into(), "OPTIONS".into()]
}

impl Default for Server {
    fn default() -> Self {
        Server::builder().build()
    }
}

impl Cors {
    pub fn into_warp_middleware(&self) -> warp::cors::Builder {
        let cors = warp::cors()
            .allow_credentials(self.allow_credentials.unwrap_or_default())
            .allow_headers(self.allow_headers.iter().map(std::string::String::as_str))
            .expose_headers(self.allow_headers.iter().map(std::string::String::as_str))
            .allow_methods(self.methods.iter().map(std::string::String::as_str));

        if self.allow_any_origin.unwrap_or(true) {
            cors.allow_any_origin()
        } else {
            cors.allow_origins(self.origins.iter().map(std::string::String::as_str))
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum OpenTelemetry {
    Jaeger(Option<Jaeger>),
    #[cfg(any(feature = "otlp-tonic", feature = "otlp-grpcio", feature = "otlp-http"))]
    Otlp(otlp::Otlp),
}

#[derive(Debug, Clone, Derivative, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[derivative(Default)]
pub struct Jaeger {
    pub collector_endpoint: Option<Url>,
    #[serde(default = "default_jaeger_service_name")]
    #[derivative(Default(value = "default_jaeger_service_name()"))]
    pub service_name: String,
    #[serde(skip, default = "default_jaeger_username")]
    #[derivative(Default(value = "default_jaeger_username()"))]
    pub username: Option<String>,
    #[serde(skip, default = "default_jaeger_password")]
    #[derivative(Default(value = "default_jaeger_password()"))]
    pub password: Option<String>,
}

fn default_jaeger_service_name() -> String {
    "router".to_string()
}

fn default_jaeger_username() -> Option<String> {
    std::env::var("JAEGER_USERNAME").ok()
}

fn default_jaeger_password() -> Option<String> {
    std::env::var("JAEGER_PASSWORD").ok()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    domain_name: Option<String>,
    ca: Option<Secret>,
    cert: Option<Secret>,
    key: Option<Secret>,
}

impl TlsConfig {
    #[cfg(feature = "tls")]
    pub fn tls_config(
        &self,
    ) -> Result<tonic::transport::channel::ClientTlsConfig, ConfigurationError> {
        let mut config = tonic::transport::channel::ClientTlsConfig::new();

        if let Some(domain_name) = self.domain_name.as_ref() {
            config = config.domain_name(domain_name);
        }

        if let Some(ca_certificate) = self.ca.as_ref() {
            let certificate = tonic::transport::Certificate::from_pem(ca_certificate.read()?);
            config = config.ca_certificate(certificate);
        }

        match (self.cert.as_ref(), self.key.as_ref()) {
            (Some(cert), Some(key)) => {
                let identity = tonic::transport::Identity::from_pem(cert.read()?, key.read()?);
                config = config.identity(identity);
            }
            _ => {}
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_config_snapshot {
        ($file:expr) => {{
            let config = serde_yaml::from_str::<Configuration>(include_str!($file)).unwrap();
            insta::with_settings!({sort_maps => true}, {
                insta::assert_yaml_snapshot!(config);
            });
        }};
    }

    #[test]
    fn test_supergraph_config_serde() {
        assert_config_snapshot!("testdata/supergraph_config.yaml");
    }

    #[test]
    fn ensure_configuration_api_does_not_change() {
        assert_config_snapshot!("testdata/config_basic.yml");
        assert_config_snapshot!("testdata/config_full.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_jaeger_basic.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_jaeger_full.yml");
    }

    #[cfg(any(feature = "otlp-tonic", feature = "otlp-grpcio", feature = "otlp-http"))]
    #[test]
    fn ensure_configuration_api_does_not_change_common() {
        // NOTE: don't take a snapshot here because the optional fields appear with ~ and they vary
        // per implementation
        serde_yaml::from_str::<Configuration>(include_str!(
            "testdata/config_opentelemetry_otlp_tracing_common.yml"
        ))
        .unwrap();
    }

    #[cfg(feature = "otlp-tonic")]
    #[test]
    fn ensure_configuration_api_does_not_change_tonic() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_tonic_basic.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_tonic_full.yml");
    }

    #[cfg(feature = "otlp-grpcio")]
    #[test]
    fn ensure_configuration_api_does_not_change_grpcio() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_grpcio_basic.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_grpcio_full.yml");
    }

    #[cfg(feature = "otlp-http")]
    #[test]
    fn ensure_configuration_api_does_not_change_http() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_http_basic.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_http_full.yml");
    }

    #[cfg(all(feature = "tls", feature = "otlp-tonic"))]
    #[test]
    fn ensure_configuration_api_does_not_change_tls_config() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tls.yml");
    }

    #[test]
    fn routing_url_compatibility_with_schema() {
        let mut configuration = Configuration::builder()
            .subgraphs(
                [
                    (
                        "inventory".to_string(),
                        Subgraph {
                            routing_url: "http://inventory/graphql".to_string(),
                        },
                    ),
                    (
                        "products".to_string(),
                        Subgraph {
                            routing_url: "http://products/graphql".to_string(),
                        },
                    ),
                ]
                .iter()
                .cloned()
                .collect(),
            )
            .build();

        let schema: Schema = r#"
        enum join__Graph {
          ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
          INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
          PRODUCTS @join__graph(name: "products" url: "")
          REVIEWS @join__graph(name: "reviews" url: "")
        }"#
        .parse()
        .unwrap();

        let res = configuration.load_subgraphs(&schema);

        // if no configuration override, use the URL from the supergraph
        assert_eq!(
            configuration.subgraphs.get("accounts").unwrap().routing_url,
            "http://localhost:4001/graphql"
        );
        // if both configuration and schema specify a non empty URL, the configuration wins
        // this should show a warning in logs
        assert_eq!(
            configuration
                .subgraphs
                .get("inventory")
                .unwrap()
                .routing_url,
            "http://localhost:4002/graphql"
        );
        // if the configuration has a non empty routing URL, and the supergraph
        // has an empty one, the schema wins
        assert_eq!(
            configuration.subgraphs.get("products").unwrap().routing_url,
            "http://products/graphql"
        );
        // if the configuration has a no routing URL, and the supergraph
        // has an empty one, it does not get into the configuration
        // and loading returns an error
        assert!(configuration.subgraphs.get("reviews").is_none());

        match res {
            Err(errors) => {
                assert_eq!(errors.len(), 1);

                if let Some(ConfigurationError::MissingSubgraphUrl(subgraph)) = errors.get(0) {
                    assert_eq!(subgraph, "reviews");
                } else {
                    panic!(
                        "expected missing subgraph URL for 'reviews', got: {:?}",
                        errors
                    );
                }
            }
            Ok(()) => panic!("expected missing subgraph URL for 'reviews'"),
        }
    }
}
