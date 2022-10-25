//! Logic for loading configuration in to an object model
// This entire file is license key functionality
pub(crate) mod cors;
mod expansion;
#[cfg(test)]
mod tests;
mod yaml;

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

use askama::Template;
use bytes::Bytes;
use derivative::Derivative;
use displaydoc::Display;
use itertools::Itertools;
use jsonschema::Draft;
use jsonschema::JSONSchema;
use schemars::gen::SchemaGenerator;
use schemars::gen::SchemaSettings;
use schemars::schema::ObjectValidation;
use schemars::schema::RootSchema;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Map;
use serde_json::Value;
use thiserror::Error;

use crate::executable::APOLLO_ROUTER_DEV_ENV;
use crate::plugin::plugins;
use cors::*;
use expansion::*;

/// Configuration error.
#[derive(Debug, Error, Display)]
#[non_exhaustive]
pub enum ConfigurationError {
    /// could not expand variable: {key}, {cause}
    CannotExpandVariable { key: String, cause: String },
    /// could not expand variable: {key}. Variables must be prefixed with one of '{supported_modes}' followed by '.' e.g. 'env.'
    UnknownExpansionMode {
        key: String,
        supported_modes: String,
    },
    /// unknown plugin {0}
    PluginUnknown(String),
    /// plugin {plugin} could not be configured: {error}
    PluginConfiguration { plugin: String, error: String },
    /// {message}: {error}
    InvalidConfiguration {
        message: &'static str,
        error: String,
    },
    /// could not deserialize configuration: {0}
    DeserializeConfigError(serde_json::Error),

    /// APOLLO_ROUTER_CONFIG_SUPPORTED_MODES must be of the format env,file,... Possible modes are 'env' and 'file'.
    InvalidExpansionModeConfig,
}

/// The configuration for the router.
///
/// Can be created through `serde::Deserialize` from various formats,
/// or inline in Rust code with `serde_json::json!` and `serde_json::from_value`.
#[derive(Clone, Derivative, Serialize, JsonSchema, Default)]
#[derivative(Debug)]
pub struct Configuration {
    /// Configuration options pertaining to the http server component.
    #[serde(default)]
    pub(crate) server: Server,

    #[serde(default)]
    #[serde(rename = "health-check")]
    pub(crate) health_check: HealthCheck,

    #[serde(default)]
    pub(crate) sandbox: Sandbox,

    #[serde(default)]
    pub(crate) homepage: Homepage,

    #[serde(default)]
    pub(crate) supergraph: Supergraph,
    /// Cross origin request headers.
    #[serde(default)]
    pub(crate) cors: Cors,

    /// Plugin configuration
    #[serde(default)]
    plugins: UserPlugins,

    /// Built-in plugin configuration. Built in plugins are pushed to the top level of config.
    #[serde(default)]
    #[serde(flatten)]
    apollo_plugins: ApolloPlugins,
}

impl<'de> serde::Deserialize<'de> for Configuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // This intermediate structure will allow us to deserialize a Configuration
        // yet still exercise the Configuration validation function
        #[derive(Deserialize, Default)]
        struct AdHocConfiguration {
            #[serde(default)]
            server: Server,
            #[serde(default)]
            #[serde(rename = "health-check")]
            health_check: HealthCheck,
            #[serde(default)]
            sandbox: Sandbox,
            #[serde(default)]
            homepage: Homepage,
            #[serde(default)]
            supergraph: Supergraph,
            #[serde(default)]
            cors: Cors,
            #[serde(default)]
            plugins: UserPlugins,
            #[serde(default)]
            #[serde(flatten)]
            apollo_plugins: ApolloPlugins,
        }
        let ad_hoc: AdHocConfiguration = serde::Deserialize::deserialize(deserializer)?;

        Configuration::builder()
            .server(ad_hoc.server)
            .health_check(ad_hoc.health_check)
            .sandbox(ad_hoc.sandbox)
            .homepage(ad_hoc.homepage)
            .supergraph(ad_hoc.supergraph)
            .cors(ad_hoc.cors)
            .plugins(ad_hoc.plugins.plugins.unwrap_or_default())
            .apollo_plugins(ad_hoc.apollo_plugins.plugins)
            .build()
            .map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

const APOLLO_PLUGIN_PREFIX: &str = "apollo.";
const TELEMETRY_KEY: &str = "telemetry";

fn default_graphql_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:4000").unwrap().into()
}

// This isn't dead code! we use it in buildstructor's fake_new
#[allow(dead_code)]
fn test_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:0").unwrap().into()
}

#[buildstructor::buildstructor]
impl Configuration {
    #[builder]
    pub(crate) fn new(
        server: Option<Server>,
        supergraph: Option<Supergraph>,
        health_check: Option<HealthCheck>,
        sandbox: Option<Sandbox>,
        homepage: Option<Homepage>,
        cors: Option<Cors>,
        plugins: Map<String, Value>,
        apollo_plugins: Map<String, Value>,
        dev: Option<bool>,
    ) -> Result<Self, ConfigurationError> {
        let mut conf = Self {
            server: server.unwrap_or_default(),
            supergraph: supergraph.unwrap_or_default(),
            health_check: health_check.unwrap_or_default(),
            sandbox: sandbox.unwrap_or_default(),
            homepage: homepage.unwrap_or_default(),
            cors: cors.unwrap_or_default(),
            plugins: UserPlugins {
                plugins: Some(plugins),
            },
            apollo_plugins: ApolloPlugins {
                plugins: apollo_plugins,
            },
        };
        if dev.unwrap_or_default()
            || std::env::var(APOLLO_ROUTER_DEV_ENV).ok().as_deref() == Some("true")
        {
            conf.enable_dev_mode();
        }

        conf.validate()
    }

    /// This should be executed after normal configuration processing
    pub(crate) fn enable_dev_mode(&mut self) {
        tracing::info!("Running with *development* mode settings which facilitate development experience (e.g., introspection enabled)");

        if self.plugins.plugins.is_none() {
            self.plugins.plugins = Some(Map::new());
        }
        self.plugins.plugins.as_mut().unwrap().insert(
            "experimental.expose_query_plan".to_string(),
            Value::Bool(true),
        );
        self.apollo_plugins
            .plugins
            .insert("include_subgraph_errors".to_string(), json!({"all": true}));
        self.supergraph.introspection = true;
        self.sandbox.enabled = true;
        self.homepage.enabled = false;
    }

    #[cfg(test)]
    pub(crate) fn boxed(self) -> Box<Self> {
        Box::new(self)
    }

    pub(crate) fn plugins(&self) -> Vec<(String, Value)> {
        let mut plugins = vec![];

        // Add all the apollo plugins
        for (plugin, config) in &self.apollo_plugins.plugins {
            let plugin_full_name = format!("{}{}", APOLLO_PLUGIN_PREFIX, plugin);
            tracing::debug!(
                "adding plugin {} with user provided configuration",
                plugin_full_name.as_str()
            );
            plugins.push((plugin_full_name, config.clone()));
        }

        // Add all the user plugins
        if let Some(config_map) = self.plugins.plugins.as_ref() {
            for (plugin, config) in config_map {
                plugins.push((plugin.clone(), config.clone()));
            }
        }

        plugins
    }

    pub(crate) fn plugin_configuration(&self, plugin_name: &str) -> Option<Value> {
        self.plugins()
            .iter()
            .find(|(name, _)| name == plugin_name)
            .map(|(_, value)| value.clone())
    }

    // checks that we can reload configuration from the current one to the new one
    pub(crate) fn is_compatible(&self, new: &Configuration) -> Result<(), &'static str> {
        if self.apollo_plugins.plugins.get(TELEMETRY_KEY)
            == new.apollo_plugins.plugins.get(TELEMETRY_KEY)
        {
            Ok(())
        } else {
            Err("incompatible telemetry configuration. Telemetry cannot be reloaded and its configuration must stay the same for the entire life of the process")
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Configuration {
    #[builder]
    pub(crate) fn fake_new(
        server: Option<Server>,
        supergraph: Option<Supergraph>,
        health_check: Option<HealthCheck>,
        sandbox: Option<Sandbox>,
        homepage: Option<Homepage>,
        cors: Option<Cors>,
        plugins: Map<String, Value>,
        apollo_plugins: Map<String, Value>,
        dev: Option<bool>,
    ) -> Result<Self, ConfigurationError> {
        let mut configuration = Self {
            server: server.unwrap_or_default(),
            supergraph: supergraph.unwrap_or_else(|| Supergraph::fake_builder().build()),
            health_check: health_check.unwrap_or_else(|| HealthCheck::fake_builder().build()),
            sandbox: sandbox.unwrap_or_else(|| Sandbox::fake_builder().build()),
            homepage: homepage.unwrap_or_else(|| Homepage::fake_builder().build()),
            cors: cors.unwrap_or_default(),
            plugins: UserPlugins {
                plugins: Some(plugins),
            },
            apollo_plugins: ApolloPlugins {
                plugins: apollo_plugins,
            },
        };
        if dev.unwrap_or_default()
            || std::env::var(APOLLO_ROUTER_DEV_ENV).ok().as_deref() == Some("true")
        {
            configuration.enable_dev_mode();
        }

        configuration.validate()
    }
}

impl Configuration {
    pub(crate) fn validate(self) -> Result<Self, ConfigurationError> {
        // Sandbox and Homepage cannot be both enabled
        if self.sandbox.enabled && self.homepage.enabled {
            return Err(ConfigurationError::InvalidConfiguration {
                message: "sandbox and homepage cannot be enabled at the same time",
                error: "disable the homepage if you want to enable sandbox".to_string(),
            });
        }
        // Sandbox needs Introspection to be enabled
        if self.sandbox.enabled && !self.supergraph.introspection {
            return Err(ConfigurationError::InvalidConfiguration {
                message: "sandbox requires introspection",
                error: "sandbox needs introspection to be enabled".to_string(),
            });
        }
        if !self.supergraph.path.starts_with('/') {
            return Err(ConfigurationError::InvalidConfiguration {
            message: "invalid 'server.graphql_path' configuration",
            error: format!(
                "'{}' is invalid, it must be an absolute path and start with '/', you should try with '/{}'",
                self.supergraph.path,
                self.supergraph.path
            ),
        });
        }
        if self.supergraph.path.ends_with('*') && !self.supergraph.path.ends_with("/*") {
            return Err(ConfigurationError::InvalidConfiguration {
                message: "invalid 'server.graphql_path' configuration",
                error: format!(
                    "'{}' is invalid, you can only set a wildcard after a '/'",
                    self.supergraph.path
                ),
            });
        }
        if self.supergraph.path.contains("/*/") {
            return Err(
                ConfigurationError::InvalidConfiguration {
                    message: "invalid 'server.graphql_path' configuration",
                    error: format!(
                        "'{}' is invalid, if you need to set a path like '/*/graphql' then specify it as a path parameter with a name, for example '/:my_project_key/graphql'",
                        self.supergraph.path
                    ),
                },
            );
        }
        Ok(self)
    }
}

/// Parse configuration from a string in YAML syntax
impl FromStr for Configuration {
    type Err = ConfigurationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        validate_yaml_configuration(s, Expansion::default()?)?.validate()
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
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct ApolloPlugins {
    pub(crate) plugins: Map<String, Value>,
}

impl JsonSchema for ApolloPlugins {
    fn schema_name() -> String {
        stringify!(Plugins).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // This is a manual implementation of Plugins schema to allow plugins that have been registered at
        // compile time to be picked up.

        let plugins = crate::plugin::plugins()
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
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct UserPlugins {
    pub(crate) plugins: Option<Map<String, Value>>,
}

impl JsonSchema for UserPlugins {
    fn schema_name() -> String {
        stringify!(Plugins).to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // This is a manual implementation of Plugins schema to allow plugins that have been registered at
        // compile time to be picked up.

        let plugins = crate::plugin::plugins()
            .iter()
            .sorted_by_key(|(name, _)| *name)
            .filter(|(name, _)| !name.starts_with(APOLLO_PLUGIN_PREFIX))
            .map(|(name, factory)| (name.to_string(), factory.create_schema(gen)))
            .collect::<schemars::Map<String, Schema>>();
        gen_schema(plugins)
    }
}

/// Configuration options pertaining to the supergraph server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Supergraph {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:4000
    #[serde(default = "default_graphql_listen")]
    pub(crate) listen: ListenAddr,

    /// The HTTP path on which GraphQL requests will be served.
    /// default: "/"
    #[serde(default = "default_graphql_path")]
    pub(crate) path: String,

    /// Enable introspection
    /// Default: false
    #[serde(default = "default_graphql_introspection")]
    pub(crate) introspection: bool,

    #[serde(default = "default_defer_support")]
    pub(crate) preview_defer_support: bool,
}

fn default_defer_support() -> bool {
    true
}

#[buildstructor::buildstructor]
impl Supergraph {
    #[builder]
    pub(crate) fn new(
        listen: Option<ListenAddr>,
        path: Option<String>,
        introspection: Option<bool>,
        preview_defer_support: Option<bool>,
    ) -> Self {
        Self {
            listen: listen.unwrap_or_else(default_graphql_listen),
            path: path.unwrap_or_else(default_graphql_path),
            introspection: introspection.unwrap_or_else(default_graphql_introspection),
            preview_defer_support: preview_defer_support.unwrap_or_else(default_defer_support),
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Supergraph {
    #[builder]
    pub(crate) fn fake_new(
        listen: Option<ListenAddr>,
        path: Option<String>,
        introspection: Option<bool>,
        preview_defer_support: Option<bool>,
    ) -> Self {
        Self {
            listen: listen.unwrap_or_else(test_listen),
            path: path.unwrap_or_else(default_graphql_path),
            introspection: introspection.unwrap_or_else(default_graphql_introspection),
            preview_defer_support: preview_defer_support.unwrap_or_else(default_defer_support),
        }
    }
}

impl Default for Supergraph {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Configuration options pertaining to the sandbox page.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sandbox {
    #[serde(default = "default_sandbox")]
    pub(crate) enabled: bool,
}

fn default_sandbox() -> bool {
    false
}

#[buildstructor::buildstructor]
impl Sandbox {
    #[builder]
    pub(crate) fn new(enabled: Option<bool>) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_sandbox),
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Sandbox {
    #[builder]
    pub(crate) fn fake_new(enabled: Option<bool>) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_sandbox),
        }
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[derive(Template)]
#[template(path = "sandbox_index.html")]
struct SandboxTemplate {}

impl Sandbox {
    pub(crate) fn display_page() -> Bytes {
        let template = SandboxTemplate {};
        template.render().unwrap().into()
    }
}

/// Configuration options pertaining to the home page.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Homepage {
    #[serde(default = "default_homepage")]
    pub(crate) enabled: bool,
}

fn default_homepage() -> bool {
    true
}

#[buildstructor::buildstructor]
impl Homepage {
    #[builder]
    pub(crate) fn new(enabled: Option<bool>) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_homepage),
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Homepage {
    #[builder]
    pub(crate) fn fake_new(enabled: Option<bool>) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_homepage),
        }
    }
}

impl Default for Homepage {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[derive(Template)]
#[template(path = "homepage_index.html")]
struct HomepageTemplate {}

impl Homepage {
    pub(crate) fn display_page() -> Bytes {
        let template = HomepageTemplate {};
        template.render().unwrap().into()
    }
}

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthCheck {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:8088
    #[serde(default = "default_health_check_listen")]
    pub(crate) listen: ListenAddr,

    #[serde(default = "default_health_check")]
    pub(crate) enabled: bool,
}

fn default_health_check_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:8088").unwrap().into()
}

fn default_health_check() -> bool {
    true
}

#[buildstructor::buildstructor]
impl HealthCheck {
    #[builder]
    pub(crate) fn new(listen: Option<ListenAddr>, enabled: Option<bool>) -> Self {
        Self {
            listen: listen.unwrap_or_else(default_health_check_listen),
            enabled: enabled.unwrap_or_else(default_health_check),
        }
    }

    // Used in tests
    #[allow(dead_code)]
    #[builder]
    pub(crate) fn fake_new(listen: Option<ListenAddr>, enabled: Option<bool>) -> Self {
        Self {
            listen: listen.unwrap_or_else(test_listen),
            enabled: enabled.unwrap_or_else(default_health_check),
        }
    }
}

impl Default for HealthCheck {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Server {
    /// Experimental limitation of query depth
    /// default: 4096
    #[serde(default = "default_parser_recursion_limit")]
    pub(crate) experimental_parser_recursion_limit: usize,
}

#[buildstructor::buildstructor]
impl Server {
    #[builder]
    #[allow(clippy::too_many_arguments)] // Used through a builder, not directly
    pub(crate) fn new(parser_recursion_limit: Option<usize>) -> Self {
        Self {
            experimental_parser_recursion_limit: parser_recursion_limit
                .unwrap_or_else(default_parser_recursion_limit),
        }
    }
}

/// Listening address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ListenAddr {
    /// Socket address.
    SocketAddr(SocketAddr),
    /// Unix socket.
    #[cfg(unix)]
    UnixSocket(std::path::PathBuf),
}

impl ListenAddr {
    pub(crate) fn ip_and_port(&self) -> Option<(IpAddr, u16)> {
        #[cfg_attr(not(unix), allow(irrefutable_let_patterns))]
        if let Self::SocketAddr(addr) = self {
            Some((addr.ip(), addr.port()))
        } else {
            None
        }
    }
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

fn default_graphql_path() -> String {
    String::from("/")
}

fn default_graphql_introspection() -> bool {
    false
}

fn default_parser_recursion_limit() -> usize {
    // This is `apollo-parser`’s default, which protects against stack overflow
    // but is still very high for "reasonable" queries.
    // https://docs.rs/apollo-parser/0.2.8/src/apollo_parser/parser/mod.rs.html#368
    4096
}

impl Default for Server {
    fn default() -> Self {
        Server::builder().build()
    }
}

/// Generate a JSON schema for the configuration.
pub(crate) fn generate_config_schema() -> RootSchema {
    let settings = SchemaSettings::draft07().with(|s| {
        s.option_nullable = true;
        s.option_add_null_type = false;
        s.inline_subschemas = true;
    });

    // Manually patch up the schema
    // We don't want to allow unknown fields, but serde doesn't work if we put the annotation on Configuration as the struct has a flattened type.
    // It's fine to just add it here.
    let gen = settings.into_generator();
    let mut schema = gen.into_root_schema_for::<Configuration>();
    let mut root = schema.schema.object.as_mut().expect("schema not generated");
    root.additional_properties = Some(Box::new(schemars::schema::Schema::Bool(false)));
    schema
}

/// Validate config yaml against the generated json schema.
/// This is a tricky problem, and the solution here is by no means complete.
/// In the case that validation cannot be performed then it will let serde validate as normal. The
/// goal is to give a good enough experience until more time can be spent making this better,
///
/// The validation sequence is:
/// 1. Parse the config into yaml
/// 2. Create the json schema
/// 3. Expand env variables
/// 3. Validate the yaml against the json schema.
/// 4. Convert the json paths from the error messages into nice error snippets. Makes sure to use the values from the original source document to prevent leaks of secrets etc.
///
/// There may still be serde validation issues later.
///
fn validate_yaml_configuration(
    raw_yaml: &str,
    expansion: Expansion,
) -> Result<Configuration, ConfigurationError> {
    let defaulted_yaml = if raw_yaml.trim().is_empty() {
        "plugins:".to_string()
    } else {
        raw_yaml.to_string()
    };

    let yaml = &serde_yaml::from_str(&defaulted_yaml).map_err(|e| {
        ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: e.to_string(),
        }
    })?;

    let expanded_yaml = expand_env_variables(yaml, expansion)?;
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
    if let Err(errors) = schema.validate(&expanded_yaml) {
        // Validation failed, translate the errors into something nice for the user
        // We have to reparse the yaml to get the line number information for each error.
        match yaml::parse(raw_yaml) {
            Ok(yaml) => {
                let yaml_split_by_lines = raw_yaml.split('\n').collect::<Vec<_>>();

                let errors = errors
                    .enumerate()
                    .filter_map(|(idx, mut e)| {
                        if let Some(element) = yaml.get_element(&e.instance_path) {
                            const NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY: usize = 5;
                            match element {
                                yaml::Value::String(value, marker) => {
                                    let lines = yaml_split_by_lines[0.max(
                                        marker
                                            .line()
                                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY),
                                    )
                                        ..marker.line()]
                                        .iter()
                                        .join("\n");

                                    // Replace the value in the error message with the one from the raw config.
                                    // This guarantees that if the env variable contained a secret it won't be leaked.
                                    e.instance = Cow::Owned(coerce(value));

                                    Some(format!(
                                        "{}. {}\n\n{}\n{}^----- {}",
                                        idx + 1,
                                        e.instance_path,
                                        lines,
                                        " ".repeat(0.max(marker.col())),
                                        e
                                    ))
                                }
                                seq_element @ yaml::Value::Sequence(_, m) => {
                                    let (start_marker, end_marker) = (m, seq_element.end_marker());

                                    let offset = 0.max(
                                        start_marker
                                            .line()
                                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY),
                                    );
                                    let lines = yaml_split_by_lines[offset..end_marker.line()]
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, line)| {
                                            let real_line = idx + offset;
                                            match real_line.cmp(&start_marker.line()) {
                                                Ordering::Equal => format!("┌ {line}"),
                                                Ordering::Greater => format!("| {line}"),
                                                Ordering::Less => line.to_string(),
                                            }
                                        })
                                        .join("\n");

                                    Some(format!(
                                        "{}. {}\n\n{}\n└-----> {}",
                                        idx + 1,
                                        e.instance_path,
                                        lines,
                                        e
                                    ))
                                }
                                map_value
                                @ yaml::Value::Mapping(current_label, _value, _marker) => {
                                    let (start_marker, end_marker) = (
                                        current_label.as_ref()?.marker.as_ref()?,
                                        map_value.end_marker(),
                                    );
                                    let offset = 0.max(
                                        start_marker
                                            .line()
                                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY),
                                    );
                                    let lines = yaml_split_by_lines[offset..end_marker.line()]
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, line)| {
                                            let real_line = idx + offset;
                                            match real_line.cmp(&start_marker.line()) {
                                                Ordering::Equal => format!("┌ {line}"),
                                                Ordering::Greater => format!("| {line}"),
                                                Ordering::Less => line.to_string(),
                                            }
                                        })
                                        .join("\n");

                                    Some(format!(
                                        "{}. {}\n\n{}\n└-----> {}",
                                        idx + 1,
                                        e.instance_path,
                                        lines,
                                        e
                                    ))
                                }
                            }
                        } else {
                            None
                        }
                    })
                    .join("\n\n");

                if !errors.is_empty() {
                    return Err(ConfigurationError::InvalidConfiguration {
                        message: "configuration had errors",
                        error: format!("\n{}", errors),
                    });
                }
            }
            Err(e) => {
                // the yaml failed to parse. Just let serde do it's thing.
                tracing::warn!(
                    "failed to parse yaml using marked parser: {}. Falling back to serde validation",
                    e
                );
            }
        }
    }

    let config: Configuration = serde_json::from_value(expanded_yaml)
        .map_err(ConfigurationError::DeserializeConfigError)?;

    // ------------- Check for unknown fields at runtime ----------------
    // We can't do it with the `deny_unknown_fields` property on serde because we are using `flatten`
    let registered_plugins = plugins();
    let apollo_plugin_names: Vec<&str> = registered_plugins
        .keys()
        .filter_map(|n| n.strip_prefix(APOLLO_PLUGIN_PREFIX))
        .collect();
    let unknown_fields: Vec<&String> = config
        .apollo_plugins
        .plugins
        .keys()
        .filter(|ap_name| {
            let ap_name = ap_name.as_str();
            ap_name != "server" && ap_name != "plugins" && !apollo_plugin_names.contains(&ap_name)
        })
        .collect();

    if !unknown_fields.is_empty() {
        return Err(ConfigurationError::InvalidConfiguration {
            message: "unknown fields",
            error: format!(
                "additional properties are not allowed ('{}' was/were unexpected)",
                unknown_fields.iter().join(", ")
            ),
        });
    }

    Ok(config)
}
