//! Logic for loading configuration in to an object model

mod yaml;

use crate::subscriber::is_global_subscriber_set;
use apollo_router_core::plugins;
use derivative::Derivative;
use displaydoc::Display;
use envmnt::{ExpandOptions, ExpansionType};
use itertools::Itertools;
use jsonschema::{Draft, JSONSchema};
use schemars::gen::{SchemaGenerator, SchemaSettings};
use schemars::schema::{ObjectValidation, RootSchema, Schema, SchemaObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Map;
use serde_json::Value;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use thiserror::Error;
use tower_http::cors::{Any, CorsLayer, Origin};
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
    /// could not setup OTLP metrics: {0}
    Metrics(#[from] opentelemetry::metrics::MetricsError),
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
    /// {message}: {error}
    InvalidConfiguration {
        message: &'static str,
        error: String,
    },
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
            serde_yaml::from_str(s).map_err(|e| ConfigurationError::InvalidConfiguration {
                message: "failed to parse configuration",
                error: e.to_string(),
            })?;
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

/// Plugins provided by Apollo.
///
/// These plugins are processed prior to user plugins. Also, their configuration
/// is "hoisted" to the top level of the config rather than being processed
/// under "plugins" as for user plugins.
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

/// Plugins provided by a user.
///
/// These plugins are compiled into a router by and their configuration is performed
/// under the "plugins" section.
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

    /// display landing page
    /// enabled by default
    #[serde(default = "default_landing_page")]
    #[builder(default_code = "default_landing_page()", setter(into))]
    pub landing_page: bool,
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
    vec!["https://studio.apollographql.com".into()]
}

fn default_cors_headers() -> Vec<String> {
    vec![
        "Content-Type".into(),
        "apollographql-client-name".into(),
        "apollographql-client-version".into(),
    ]
}

fn default_cors_methods() -> Vec<String> {
    vec!["GET".into(), "POST".into(), "OPTIONS".into()]
}

fn default_introspection() -> bool {
    true
}

fn default_landing_page() -> bool {
    true
}

impl Default for Server {
    fn default() -> Self {
        Server::builder().build()
    }
}

impl Cors {
    pub fn into_layer(self) -> CorsLayer {
        let cors =
            CorsLayer::new()
                .allow_credentials(self.allow_credentials.unwrap_or_default())
                .allow_headers(self.allow_headers.iter().filter_map(|header| {
                    header
                        .parse()
                        .map_err(|_| tracing::error!("header name '{header}' is not valid"))
                        .ok()
                }))
                .expose_headers(self.expose_headers.unwrap_or_default().iter().filter_map(
                    |header| {
                        header
                            .parse()
                            .map_err(|_| tracing::error!("header name '{header}' is not valid"))
                            .ok()
                    },
                ))
                .allow_methods(self.methods.iter().filter_map(|method| {
                    method
                        .parse()
                        .map_err(|_| tracing::error!("method '{method}' is not valid"))
                        .ok()
                }));

        if self.allow_any_origin.unwrap_or_default() {
            cors.allow_origin(Any)
        } else {
            cors.allow_origin(Origin::list(self.origins.into_iter().filter_map(
                |origin| {
                    origin
                        .parse()
                        .map_err(|_| tracing::error!("origin '{origin}' is not valid"))
                        .ok()
                },
            )))
        }
    }
}

pub fn generate_config_schema() -> RootSchema {
    let settings = SchemaSettings::draft07().with(|s| {
        s.option_nullable = true;
        s.option_add_null_type = false;
        s.inline_subschemas = true;
    });
    let gen = settings.into_generator();
    gen.into_root_schema_for::<Configuration>()
}

/// Validate config yaml against the generated json schema.
/// This is a tricky problem, and the solution here is by no means complete.
/// In the case that validation cannot be performed then it will let serde validate as normal. The
/// goal is to give a good enough experience until more time can be spent making this better,
///
/// THe validation sequence is:
/// 1. Parse the config into yaml
/// 2. Create the json schema
/// 3. Validate the yaml against the json schema.
/// 4. If there were errors then try and parse using a custom parser that retains line and column number info.
/// 5. Convert the json paths from the error messages into nice error snippets.
///
/// If at any point something doesn't work out it lets the config pass and it'll get re-validated by serde later.
///
pub fn validate_configuration(raw_yaml: &str) -> Result<(), ConfigurationError> {
    let yaml =
        &serde_yaml::from_str(raw_yaml).map_err(|e| ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: e.to_string(),
        })?;
    let schema = serde_json::to_value(generate_config_schema()).map_err(|e| {
        ConfigurationError::InvalidConfiguration {
            message: "failed to parse schema",
            error: e.to_string(),
        }
    })?;
    let schema = JSONSchema::options()
        .with_draft(Draft::Draft7)
        .compile(&schema)
        .map_err(|e| ConfigurationError::InvalidConfiguration {
            message: "failed to compile schema",
            error: e.to_string(),
        })?;
    if let Err(errors) = schema.validate(yaml) {
        // Validation failed, translate the errors into something nice for the user
        // We have to reparse the yaml to get the line number information for each error.
        match yaml::parse(raw_yaml) {
            Ok(yaml) => {
                let yaml_split_by_lines = raw_yaml.split('\n').collect::<Vec<_>>();

                let errors = errors
                    .enumerate()
                    .filter_map(|(idx, e)| {
                        if let Some(element) = yaml.get_element(&e.instance_path) {
                            // Dirty hack.
                            // If the element is a string and it has env variable expansion then we can't validate
                            // Leave it up to serde to catch these.
                            if let yaml::Value::String(value, _) = element {
                                if &envmnt::expand(
                                    value,
                                    Some(ExpandOptions::new().clone_with_expansion_type(
                                        ExpansionType::UnixBracketsWithDefaults,
                                    )),
                                ) != value
                                {
                                    return None;
                                }
                            }

                            let marker = element.marker();
                            let lines = yaml_split_by_lines
                                [0.max(marker.line() as isize - 5) as usize..marker.line()]
                                .iter()
                                .join("\n");

                            Some(format!(
                                "{}. {}\n\n{}\n{}^----- {}",
                                idx + 1,
                                e.instance_path,
                                lines,
                                " ".repeat(marker.col()),
                                e
                            ))
                        } else {
                            None
                        }
                    })
                    .join("\n\n");

                // There were no remaining errors after expansion.
                if errors.is_empty() {
                    return Ok(());
                }

                return Err(ConfigurationError::InvalidConfiguration {
                    message: "configuration had errors",
                    error: format!("\n{}", errors),
                });
            }
            Err(e) => {
                // the yaml failed to parse. Just let serde do it's thing.
                tracing::warn!(
                    "failed to parse yaml using marked parser: {}. Falling back to serde validation",
                    e
                );
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_router_core::prelude::*;
    use apollo_router_core::SchemaError;
    use http::Uri;
    #[cfg(unix)]
    use insta::assert_json_snapshot;
    use regex::Regex;
    #[cfg(unix)]
    use schemars::gen::SchemaSettings;
    use std::collections::HashMap;
    use std::fs;
    use walkdir::WalkDir;

    #[cfg(unix)]
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

        let subgraphs: HashMap<&String, &Uri> = schema.subgraphs().collect();

        // if no configuration override, use the URL from the supergraph
        assert_eq!(
            subgraphs.get(&"accounts".to_string()).unwrap().to_string(),
            "http://localhost:4001/graphql"
        );
        // if both configuration and schema specify a non empty URL, the configuration wins
        // this should show a warning in logs
        assert_eq!(
            subgraphs.get(&"inventory".to_string()).unwrap().to_string(),
            "http://localhost:4002/graphql"
        );
        // if the configuration has a non empty routing URL, and the supergraph
        // has an empty one, the configuration wins
        assert_eq!(
            subgraphs.get(&"products".to_string()).unwrap().to_string(),
            "http://localhost:4003/graphql"
        );

        assert_eq!(
            subgraphs.get(&"reviews".to_string()).unwrap().to_string(),
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
            ["https://studio.apollographql.com"],
            cors.origins.as_slice()
        );
        assert!(
            !cors.allow_any_origin.unwrap_or_default(),
            "Allow any origin should be disabled by default"
        );
    }

    #[test]
    fn line_precise_config_errors() {
        let error = validate_configuration(
            r#"
plugins:
  non_existant:
    foo: "bar"

telemetry:  
  another_non_existant: 3
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn validate_project_config_files() {
        let filename_matcher = Regex::from_str("((.+[.])?router\\.yaml)|(.+\\.mdx)").unwrap();
        let embedded_yaml_matcher =
            Regex::from_str(r#"(?ms)```yaml title="router.yaml"(.+?)```"#).unwrap();
        for entry in WalkDir::new("..")
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry
                .path()
                .with_file_name(".skipconfigvalidation")
                .exists()
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if filename_matcher.is_match(&name) {
                let config = fs::read_to_string(entry.path()).expect("failed to read file");
                let yamls = if name.ends_with(".mdx") {
                    // Extract yaml from docs
                    embedded_yaml_matcher
                        .captures_iter(&config)
                        .map(|i| i.get(1).unwrap().as_str().into())
                        .collect()
                } else {
                    vec![config]
                };

                for yaml in yamls {
                    if let Err(e) = validate_configuration(&yaml) {
                        panic!(
                            "{} configuration error: \n{}",
                            entry.path().to_string_lossy(),
                            e
                        )
                    }
                }
            }
        }
    }
}
