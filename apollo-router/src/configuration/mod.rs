//! Logic for loading configuration in to an object model

#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
pub mod otlp;

use crate::apollo_telemetry::{DEFAULT_LISTEN, DEFAULT_SERVER_URL};
use apollo_router_core::prelude::*;
use apollo_router_core::{layers, plugins};
use derivative::Derivative;
use displaydoc::Display;
use opentelemetry::sdk::trace::Sampler;
use opentelemetry::sdk::Resource;
use opentelemetry::KeyValue;
use reqwest::Url;
use schemars::gen::SchemaGenerator;
use schemars::schema::{
    ArrayValidation, ObjectValidation, Schema, SchemaObject, SingleOrVec, SubschemaValidation,
};
use schemars::{JsonSchema, Set};
use serde::{Deserialize, Serialize};
use serde_json::Map;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use typed_builder::TypedBuilder;

/// Configuration error.
#[derive(Debug, Error, Display)]
pub enum ConfigurationError {
    /// Could not read secret from file: {0}
    CannotReadSecretFromFile(std::io::Error),
    /// Could not read secret from environment variable: {0}
    CannotReadSecretFromEnv(std::env::VarError),
    /// Missing environment variable: {0}
    MissingEnvironmentVariable(String),
    /// Invalid environment variable: {0}
    InvalidEnvironmentVariable(String),
    /// Could not setup OTLP tracing: {0}
    OtlpTracing(opentelemetry::trace::TraceError),
    /// The configuration could not be loaded because it requires the feature {0:?}
    MissingFeature(&'static str),
    /// Could not find an URL for subgraph {0}
    MissingSubgraphUrl(String),
    /// Invalid URL for subgraph {subgraph}: {url}
    InvalidSubgraphUrl { subgraph: String, url: String },
    /// Unknown plugin {0}
    PluginUnknown(String),
    /// Plugin {plugin} could not be configured: {error}
    PluginConfiguration { plugin: String, error: String },
    /// Unknown payer {0}
    LayerUnknown(String),
    /// Layer {layer} could not be configured: {error}
    LayerConfiguration { layer: String, error: String },
    /// The configuration contained errors.
    InvalidConfiguration,
}

/// The configuration for the router.
/// Currently maintains a mapping of subgraphs.
#[derive(Clone, Derivative, Deserialize, Serialize, TypedBuilder, JsonSchema)]
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

    /// Plugin configuration
    #[serde(default)]
    #[builder(default)]
    pub plugins: Plugins,

    /// Spaceport configuration.
    #[serde(default)]
    #[builder(default)]
    pub spaceport: Option<SpaceportConfig>,

    /// Studio Graph configuration.
    #[serde(default)]
    #[builder(default)]
    pub graph: Option<StudioGraph>,
}

fn default_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:4000").unwrap().into()
}

impl Configuration {
    pub fn load_subgraphs(
        &mut self,
        schema: &graphql::Schema,
    ) -> Result<(), Vec<ConfigurationError>> {
        let mut errors = Vec::new();

        for (name, schema_url) in schema.subgraphs() {
            match self.subgraphs.get(name) {
                None => {
                    if schema_url.is_empty() {
                        errors.push(ConfigurationError::MissingSubgraphUrl(name.to_owned()));
                        continue;
                    }
                    match Url::parse(schema_url) {
                        Err(_e) => {
                            errors.push(ConfigurationError::InvalidSubgraphUrl {
                                subgraph: name.to_owned(),
                                url: schema_url.to_owned(),
                            });
                        }
                        Ok(routing_url) => {
                            self.subgraphs.insert(
                                name.to_owned(),
                                Subgraph {
                                    routing_url,
                                    layers: Vec::new(),
                                },
                            );
                        }
                    }
                }
                Some(subgraph) => {
                    if !schema_url.is_empty() && schema_url != subgraph.routing_url.as_str() {
                        tracing::warn!("overriding URL from subgraph {} at {} with URL from the configuration file: {}",
                name, schema_url, subgraph.routing_url);
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

    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, TypedBuilder)]
#[serde(transparent)]
pub struct Plugins {
    pub plugins: Map<String, Value>,
}

impl JsonSchema for Plugins {
    fn schema_name() -> String {
        stringify!(Plugins).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // This is a manual implementation of Plugins schema to allow plugins that have been registered at
        // compile time to be picked up.

        let plugins = plugins()
            .iter()
            .map(|(name, factory)| (name.to_string(), factory.create_schema(gen)))
            .collect::<schemars::Map<String, Schema>>();
        let plugins_refs = plugins
            .keys()
            .map(|name| {
                Schema::Object(SchemaObject {
                    object: Some(Box::new(ObjectValidation {
                        required: Set::from([name.to_string()]),
                        ..Default::default()
                    })),
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        let plugins_object = SchemaObject {
            object: Some(Box::new(ObjectValidation {
                properties: plugins,
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                any_of: Some(plugins_refs),
                ..Default::default()
            })),
            ..Default::default()
        };

        Schema::Object(plugins_object)
    }
}

/// Configuration for a subgraph.
#[derive(Debug, Clone, Deserialize, Serialize, TypedBuilder)]
pub struct Subgraph {
    /// The url for the subgraph.
    pub routing_url: Url,

    /// Layer configuration
    #[serde(default)]
    #[builder(default)]
    pub layers: Vec<Value>,
}

impl JsonSchema for Subgraph {
    fn schema_name() -> String {
        stringify!(Subgraph).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // This is a manual implementation of Subgraph schema to allow layers that have been registered at
        // compile time to be picked up.
        let mut subgraph = SchemaObject::default();

        let layers = layers()
            .iter()
            .map(|(name, factory)| (name.to_string(), factory.create_schema(gen)))
            .collect::<schemars::Map<String, Schema>>();
        let layer_refs = layers
            .keys()
            .map(|name| {
                Schema::Object(SchemaObject {
                    object: Some(Box::new(ObjectValidation {
                        required: Set::from([name.to_string()]),
                        ..Default::default()
                    })),
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        let layer_object = SchemaObject {
            object: Some(Box::new(ObjectValidation {
                properties: layers,
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(layer_refs),
                ..Default::default()
            })),
            ..Default::default()
        };

        let layer_array = ArrayValidation {
            items: Some(SingleOrVec::Single(Box::new(Schema::Object(layer_object)))),
            ..Default::default()
        };

        let layers_property = SchemaObject {
            array: Some(Box::new(layer_array)),
            ..SchemaObject::default()
        };

        subgraph.object = Some(Box::new(ObjectValidation {
            properties: schemars::Map::from([(
                "layers".to_string(),
                Schema::Object(layers_property),
            )]),
            ..Default::default()
        }));
        Schema::Object(subgraph)
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

fn default_collector() -> String {
    DEFAULT_SERVER_URL.to_string()
}

fn default_listener() -> String {
    DEFAULT_LISTEN.to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SpaceportConfig {
    pub(crate) external: bool,

    #[serde(default = "default_collector")]
    pub(crate) collector: String,

    #[serde(default = "default_listener")]
    pub(crate) listener: String,
}

#[derive(Clone, Derivative, Deserialize, Serialize, JsonSchema)]
#[derivative(Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct StudioGraph {
    #[serde(skip, default = "apollo_graph_reference")]
    pub(crate) reference: String,

    #[serde(skip, default = "apollo_key")]
    #[derivative(Debug = "ignore")]
    pub(crate) key: String,
}

fn apollo_key() -> String {
    std::env::var("APOLLO_KEY")
        .expect("cannot set up usage reporting if the APOLLO_KEY environment variable is not set")
}

fn apollo_graph_reference() -> String {
    match std::env::var("APOLLO_GRAPH_REF") {
        Ok(graph_ref) => graph_ref,
        Err(_) => {
            let graph_id = std::env::var("APOLLO_GRAPH_ID")
                .expect("no APOLLO_GRAPH_REF or APOLLO_GRAPH_ID environment variables");
            let variant = match std::env::var("APOLLO_GRAPH_VARIANT") {
                Ok(variant) => variant,
                Err(_) => {
                    tracing::info!("No graph variant provided. Defaulting to `current`");
                    "current".to_string()
                }
            };
            format!("{}@{}", graph_id, variant)
        }
    }
}

impl Default for SpaceportConfig {
    fn default() -> Self {
        Self {
            collector: default_collector(),
            listener: default_listener(),
            external: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum OpenTelemetry {
    Jaeger(Option<Jaeger>),
    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    Otlp(otlp::Otlp),
}

// This short circuits the Opentelemetry schema generation.
// When Otel is moved to a plugin this will be removed.
impl JsonSchema for OpenTelemetry {
    fn schema_name() -> String {
        stringify!(OpenTelemetry).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        gen.subschema_for::<OpenTelemetry>()
    }
}

#[derive(Debug, Clone, Derivative, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[derivative(Default)]
pub struct Jaeger {
    pub endpoint: Option<JaegerEndpoint>,
    #[serde(default = "default_service_name")]
    #[derivative(Default(value = "default_service_name()"))]
    pub service_name: String,
    #[serde(skip, default = "default_jaeger_username")]
    #[derivative(Default(value = "default_jaeger_username()"))]
    pub username: Option<String>,
    #[serde(skip, default = "default_jaeger_password")]
    #[derivative(Default(value = "default_jaeger_password()"))]
    pub password: Option<String>,
    pub trace_config: Option<TraceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum JaegerEndpoint {
    Agent(SocketAddr),
    Collector(Url),
}

fn default_service_name() -> String {
    "router".to_string()
}

fn default_service_namespace() -> String {
    "apollo".to_string()
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

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceConfig {
    pub sampler: Option<Sampler>,
    pub max_events_per_span: Option<u32>,
    pub max_attributes_per_span: Option<u32>,
    pub max_links_per_span: Option<u32>,
    pub max_attributes_per_event: Option<u32>,
    pub max_attributes_per_link: Option<u32>,
    pub resource: Option<Resource>,
}

impl TraceConfig {
    pub fn trace_config(&self) -> opentelemetry::sdk::trace::Config {
        let mut trace_config = opentelemetry::sdk::trace::config();
        if let Some(sampler) = self.sampler.clone() {
            let sampler: opentelemetry::sdk::trace::Sampler = sampler;
            trace_config = trace_config.with_sampler(sampler);
        }
        if let Some(n) = self.max_events_per_span {
            trace_config = trace_config.with_max_events_per_span(n);
        }
        if let Some(n) = self.max_attributes_per_span {
            trace_config = trace_config.with_max_attributes_per_span(n);
        }
        if let Some(n) = self.max_links_per_span {
            trace_config = trace_config.with_max_links_per_span(n);
        }
        if let Some(n) = self.max_attributes_per_event {
            trace_config = trace_config.with_max_attributes_per_event(n);
        }
        if let Some(n) = self.max_attributes_per_link {
            trace_config = trace_config.with_max_attributes_per_link(n);
        }

        let resource = self
            .resource
            .as_ref()
            .map(|r| {
                Resource::new(
                    r.clone()
                        .into_iter()
                        .map(|(k, v)| KeyValue::new(k, v))
                        .collect::<Vec<KeyValue>>(),
                )
            })
            .unwrap_or_else(|| {
                Resource::new(vec![
                    KeyValue::new("service.name", default_service_name()),
                    KeyValue::new("service.namespace", default_service_namespace()),
                ])
            });

        trace_config = trace_config.with_resource(resource);

        trace_config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_json_snapshot;
    use schemars::gen::SchemaSettings;

    macro_rules! assert_config_snapshot {
        ($file:expr) => {{
            let config = serde_yaml::from_str::<Configuration>(include_str!($file)).unwrap();
            insta::with_settings!({sort_maps => true}, {
                insta::assert_yaml_snapshot!(config);
            });
        }};
    }

    #[cfg(unix)]
    #[test]
    fn schema_generation() {
        let settings = SchemaSettings::draft2019_09().with(|s| {
            s.option_nullable = true;
            s.option_add_null_type = false;
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
    fn routing_url_compatibility_with_schema() {
        let mut configuration = Configuration::builder()
            .subgraphs(
                [
                    (
                        "inventory".to_string(),
                        Subgraph {
                            routing_url: Url::parse("http://inventory/graphql").unwrap(),
                            layers: Vec::new(),
                        },
                    ),
                    (
                        "products".to_string(),
                        Subgraph {
                            routing_url: Url::parse("http://products/graphql").unwrap(),
                            layers: Vec::new(),
                        },
                    ),
                ]
                .iter()
                .cloned()
                .collect(),
            )
            .build();

        let schema: graphql::Schema = r#"
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
            configuration
                .subgraphs
                .get("accounts")
                .unwrap()
                .routing_url
                .as_str(),
            "http://localhost:4001/graphql"
        );
        // if both configuration and schema specify a non empty URL, the configuration wins
        // this should show a warning in logs
        assert_eq!(
            configuration
                .subgraphs
                .get("inventory")
                .unwrap()
                .routing_url
                .as_str(),
            "http://inventory/graphql"
        );
        // if the configuration has a non empty routing URL, and the supergraph
        // has an empty one, the configuration wins
        assert_eq!(
            configuration
                .subgraphs
                .get("products")
                .unwrap()
                .routing_url
                .as_str(),
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

    #[test]
    fn invalid_subgraph_url() {
        let err = serde_yaml::from_str::<Configuration>(include_str!("testdata/invalid_url.yaml"))
            .unwrap_err();

        assert_eq!(err.to_string(), "subgraphs.accounts.routing_url: invalid value: string \"abcd\", expected relative URL without a base at line 5 column 18");
    }
}
