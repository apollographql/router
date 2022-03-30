//! Logic for loading configuration in to an object model

use crate::subscriber::is_global_subscriber_set;
use apollo_router_core::plugins;
use derivative::Derivative;
use displaydoc::Display;
use itertools::Itertools;
use schemars::gen::SchemaGenerator;
use schemars::schema::{ObjectValidation, Schema, SchemaObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Map;
use serde_json::Value;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use thiserror::Error;
use typed_builder::TypedBuilder;

/// Configuration error.
#[derive(Debug, Error, Display)]
pub enum ConfigurationError {
    /// could not read secret from file: {0}
    CannotReadSecretFromFile(std::io::Error),
    /// could not read secret from environment variable: {0}
    CannotReadSecretFromEnv(std::env::VarError),
    /// missing environment variable: {0}
    MissingEnvironmentVariable(String),
    /// invalid environment variable: {0}
    InvalidEnvironmentVariable(String),
    /// could not setup OTLP tracing: {0}
    OtlpTracing(opentelemetry::trace::TraceError),
    /// the configuration could not be loaded because it requires the feature {0:?}
    MissingFeature(&'static str),
    /// unknown plugin {0}
    PluginUnknown(String),
    /// plugin {plugin} could not be configured: {error}
    PluginConfiguration { plugin: String, error: String },
    /// plugin {plugin} could not be started: {error}
    PluginStartup { plugin: String, error: String },
    /// plugin {plugin} could not be stopped: {error}
    PluginShutdown { plugin: String, error: String },
    /// unknown layer {0}
    LayerUnknown(String),
    /// layer {layer} could not be configured: {error}
    LayerConfiguration { layer: String, error: String },
    /// the configuration contained errors
    InvalidConfiguration,
}

/// The configuration for the router.
/// Currently maintains a mapping of subgraphs.
#[derive(Clone, Derivative, Deserialize, Serialize, TypedBuilder, JsonSchema)]
#[derivative(Debug)]
pub struct Configuration {
    /// Configuration options pertaining to the http server component.
    #[serde(default)]
    #[builder(default)]
    pub server: Server,

    /// Plugin configuration
    #[serde(default)]
    #[builder(default)]
    plugins: UserPlugins,

    /// Built-in plugin configuration. Built in plugins are pushed to the top level of config.
    #[serde(default)]
    #[builder(default)]
    #[serde(flatten)]
    apollo_plugins: ApolloPlugins,
}

const APOLLO_PLUGIN_PREFIX: &str = "apollo.";

fn default_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:4000").unwrap().into()
}

impl Configuration {
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }

    pub fn plugins(&self) -> Map<String, Value> {
        let mut plugins = Vec::default();

        if is_global_subscriber_set() {
            // Add the reporting plugin, this will be overridden if such a plugin actually exists in the config.
            // Note that this can only be done if the global subscriber has been set, i.e. we're not unit testing.
            plugins.push(("apollo.telemetry".into(), Value::Object(Map::new())));
        }

        // Add all the apollo plugins
        for (plugin, config) in &self.apollo_plugins.plugins {
            plugins.push((
                format!("{}{}", APOLLO_PLUGIN_PREFIX, plugin),
                config.clone(),
            ));
        }

        // Add all the user plugins
        if let Some(config_map) = self.plugins.plugins.as_ref() {
            for (plugin, config) in config_map {
                plugins.push((plugin.clone(), config.clone()));
            }
        }

        // Plugins must be sorted. For now this sort is hard coded, but we may add something generic.
        plugins.sort_by_key(|(name, _)| match name.as_str() {
            "apollo.telemetry" => -100,
            "apollo.rhai" => 100,
            _ => 0,
        });

        let mut final_plugins = Map::new();
        for (plugin, config) in plugins {
            final_plugins.insert(plugin, config);
        }

        final_plugins
    }
}

impl FromStr for Configuration {
    type Err = ConfigurationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let config =
            serde_yaml::from_str(s).map_err(|_| ConfigurationError::InvalidConfiguration)?;
        Ok(config)
    }
}

fn gen_schema(plugins: schemars::Map<String, Schema>) -> Schema {
    let plugins_object = SchemaObject {
        object: Some(Box::new(ObjectValidation {
            properties: plugins,
            additional_properties: Option::Some(Box::new(Schema::Bool(false))),
            ..Default::default()
        })),
        ..Default::default()
    };

    Schema::Object(plugins_object)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, TypedBuilder)]
#[serde(transparent)]
pub struct ApolloPlugins {
    pub plugins: Map<String, Value>,
}

impl JsonSchema for ApolloPlugins {
    fn schema_name() -> String {
        stringify!(Plugins).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // This is a manual implementation of Plugins schema to allow plugins that have been registered at
        // compile time to be picked up.

        let plugins = plugins()
            .iter()
            .sorted_by_key(|(name, _)| *name)
            .filter(|(name, _)| name.starts_with(APOLLO_PLUGIN_PREFIX))
            .map(|(name, factory)| {
                (
                    name[APOLLO_PLUGIN_PREFIX.len()..].to_string(),
                    factory.create_schema(gen),
                )
            })
            .collect::<schemars::Map<String, Schema>>();
        gen_schema(plugins)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, TypedBuilder)]
#[serde(transparent)]
pub struct UserPlugins {
    pub plugins: Option<Map<String, Value>>,
}

impl JsonSchema for UserPlugins {
    fn schema_name() -> String {
        stringify!(Plugins).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // This is a manual implementation of Plugins schema to allow plugins that have been registered at
        // compile time to be picked up.

        let plugins = plugins()
            .iter()
            .sorted_by_key(|(name, _)| *name)
            .filter(|(name, _)| !name.starts_with(APOLLO_PLUGIN_PREFIX))
            .map(|(name, factory)| (name.to_string(), factory.create_schema(gen)))
            .collect::<schemars::Map<String, Schema>>();
        gen_schema(plugins)
    }
}

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, TypedBuilder, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:4000
    #[serde(default = "default_listen")]
    #[builder(default_code = "default_listen()", setter(into))]
    pub listen: ListenAddr,

    /// Cross origin request headers.
    #[serde(default)]
    #[builder(default)]
    pub cors: Option<Cors>,

    /// introspection queries
    /// enabled by default
    #[serde(default = "default_introspection")]
    #[builder(default_code = "default_introspection()", setter(into))]
    pub introspection: bool,
}

/// Listening address.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ListenAddr {
    /// Socket address.
    SocketAddr(SocketAddr),
    /// Unix socket.
    #[cfg(unix)]
    UnixSocket(PathBuf),
}

impl From<SocketAddr> for ListenAddr {
    fn from(addr: SocketAddr) -> Self {
        Self::SocketAddr(addr)
    }
}

#[cfg(unix)]
impl From<tokio_util::either::Either<std::net::SocketAddr, tokio::net::unix::SocketAddr>>
    for ListenAddr
{
    fn from(
        addr: tokio_util::either::Either<std::net::SocketAddr, tokio::net::unix::SocketAddr>,
    ) -> Self {
        match addr {
            tokio_util::either::Either::Left(addr) => Self::SocketAddr(addr),
            tokio_util::either::Either::Right(addr) => Self::UnixSocket(
                addr.as_pathname()
                    .map(ToOwned::to_owned)
                    .unwrap_or_default(),
            ),
        }
    }
}

impl fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::SocketAddr(addr) => write!(f, "http://{}", addr),
            #[cfg(unix)]
            Self::UnixSocket(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Cross origin request configuration.
#[derive(Debug, Clone, Deserialize, Serialize, TypedBuilder, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cors {
    #[serde(default)]
    #[builder(default)]
    /// Set to true to allow any origin.
    ///
    /// Defaults to false
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
    /// Defaults to `https://studio.apollographql.com/` for Apollo Studio.
    #[serde(default)]
    #[builder(default_code = "default_origins()")]
    pub origins: Vec<String>,

    /// Allowed request methods. Defaults to GET, POST, OPTIONS.
    #[serde(default = "default_cors_methods")]
    #[builder(default_code = "default_cors_methods()")]
    pub methods: Vec<String>,
}

fn default_origins() -> Vec<String> {
    vec!["https://studio.apollographql.com/".into()]
}

fn default_cors_headers() -> Vec<String> {
    vec!["Content-Type".into()]
}

fn default_cors_methods() -> Vec<String> {
    vec!["GET".into(), "POST".into(), "OPTIONS".into()]
}

fn default_introspection() -> bool {
    true
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

        if self.allow_any_origin.unwrap_or_default() {
            cors.allow_any_origin()
        } else {
            cors.allow_origins(self.origins.iter().map(std::string::String::as_str))
        }
    }
}

pub(crate) fn default_service_name() -> String {
    "router".to_string()
}

pub(crate) fn default_service_namespace() -> String {
    "apollo".to_string()
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

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    domain_name: Option<String>,
    ca: Option<Secret>,
    cert: Option<Secret>,
    key: Option<Secret>,
}

#[cfg(feature = "otlp-grpc")]
impl TlsConfig {
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

        if let (Some(cert), Some(key)) = (self.cert.as_ref(), self.key.as_ref()) {
            let identity = tonic::transport::Identity::from_pem(cert.read()?, key.read()?);
            config = config.identity(identity);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_router_core::prelude::*;
    use apollo_router_core::SchemaError;
    #[cfg(unix)]
    #[cfg(any(feature = "otlp-grpc"))]
    use insta::assert_json_snapshot;
    use reqwest::Url;
    #[cfg(unix)]
    #[cfg(any(feature = "otlp-grpc"))]
    use schemars::gen::SchemaSettings;
    use std::collections::HashMap;

    macro_rules! assert_config_snapshot {
        ($file:expr) => {{
            let config = serde_yaml::from_str::<Configuration>(include_str!($file)).unwrap();
            insta::with_settings!({sort_maps => true}, {
                insta::assert_yaml_snapshot!(config);
            });
        }};
    }

    #[cfg(unix)]
    #[cfg(any(feature = "otlp-grpc"))]
    #[test]
    fn schema_generation() {
        let settings = SchemaSettings::draft2019_09().with(|s| {
            s.option_nullable = true;
            s.option_add_null_type = false;
            s.inline_subschemas = true;
        });
        let gen = settings.into_generator();
        let schema = gen.into_root_schema_for::<Configuration>();
        assert_json_snapshot!(&schema)
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

    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    #[test]
    fn ensure_configuration_api_does_not_change_common() {
        // NOTE: don't take a snapshot here because the optional fields appear with ~ and they vary
        // per implementation

        #[cfg(feature = "otlp-http")]
        serde_yaml::from_str::<Configuration>(include_str!(
            "testdata/config_opentelemetry_otlp_tracing_http_common.yml"
        ))
        .unwrap();

        #[cfg(feature = "otlp-grpc")]
        serde_yaml::from_str::<Configuration>(include_str!(
            "testdata/config_opentelemetry_otlp_tracing_grpc_common.yml"
        ))
        .unwrap();
    }

    #[cfg(feature = "otlp-grpc")]
    #[test]
    fn ensure_configuration_api_does_not_change_grpc() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_grpc_basic.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_grpc_full.yml");
    }

    #[cfg(feature = "otlp-http")]
    #[test]
    fn ensure_configuration_api_does_not_change_http() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_http_basic.yml");
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_http_full.yml");
    }

    #[cfg(all(feature = "otlp-grpc"))]
    #[test]
    fn ensure_configuration_api_does_not_change_tls_config() {
        assert_config_snapshot!("testdata/config_opentelemetry_otlp_tracing_grpc_tls.yml");
    }

    #[test]
    fn routing_url_in_schema() {
        let schema: graphql::Schema = r#"
        schema
          @core(feature: "https://specs.apollo.dev/core/v0.1"),
          @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
          query: Query
        }
        
        type Query {
          me: String
        }
        
        directive @core(feature: String!) repeatable on SCHEMA
        
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        enum join__Graph {
          ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
          INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
          PRODUCTS @join__graph(name: "products" url: "http://localhost:4003/graphql")
          REVIEWS @join__graph(name: "reviews" url: "http://localhost:4004/graphql")
        }"#
        .parse()
        .unwrap();

        let subgraphs: HashMap<&String, &Url> = schema.subgraphs().collect();

        // if no configuration override, use the URL from the supergraph
        assert_eq!(
            subgraphs.get(&"accounts".to_string()).unwrap().as_str(),
            "http://localhost:4001/graphql"
        );
        // if both configuration and schema specify a non empty URL, the configuration wins
        // this should show a warning in logs
        assert_eq!(
            subgraphs.get(&"inventory".to_string()).unwrap().as_str(),
            "http://localhost:4002/graphql"
        );
        // if the configuration has a non empty routing URL, and the supergraph
        // has an empty one, the configuration wins
        assert_eq!(
            subgraphs.get(&"products".to_string()).unwrap().as_str(),
            "http://localhost:4003/graphql"
        );

        assert_eq!(
            subgraphs.get(&"reviews".to_string()).unwrap().as_str(),
            "http://localhost:4004/graphql"
        );
    }

    #[test]
    fn missing_subgraph_url() {
        let schema_error = r#"
        schema
          @core(feature: "https://specs.apollo.dev/core/v0.1"),
          @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
          query: Query
        }
        
        type Query {
          me: String
        }
        
        directive @core(feature: String!) repeatable on SCHEMA
        
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        
        enum join__Graph {
          ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
          INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
          PRODUCTS @join__graph(name: "products" url: "http://localhost:4003/graphql")
          REVIEWS @join__graph(name: "reviews" url: "")
        }"#
        .parse::<graphql::Schema>()
        .expect_err("Must have an error because we have one missing subgraph routing url");

        if let SchemaError::MissingSubgraphUrl(subgraph) = schema_error {
            assert_eq!(subgraph, "reviews");
        } else {
            panic!(
                "expected missing subgraph URL for 'reviews', got: {:?}",
                schema_error
            );
        }
    }

    #[test]
    fn cors_defaults() {
        let cors = Cors::builder().build();

        assert_eq!(
            ["https://studio.apollographql.com/"],
            cors.origins.as_slice()
        );
        assert!(
            !cors.allow_any_origin.unwrap_or_default(),
            "Allow any origin should be disabled by default"
        );
    }
}
