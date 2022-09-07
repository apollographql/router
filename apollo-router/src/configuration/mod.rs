//! Logic for loading configuration in to an object model
// This entire file is license key functionality
mod yaml;

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use derivative::Derivative;
use displaydoc::Display;
use envmnt::ExpandOptions;
use envmnt::ExpansionType;
use http::request::Parts;
use http::HeaderValue;
use itertools::Itertools;
use jsonschema::Draft;
use jsonschema::JSONSchema;
use regex::Regex;
use schemars::gen::SchemaGenerator;
use schemars::gen::SchemaSettings;
use schemars::schema::ObjectValidation;
use schemars::schema::RootSchema;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use thiserror::Error;
use tower_http::cors::CorsLayer;
use tower_http::cors::{self};

use crate::plugin::plugins;

/// Configuration error.
#[derive(Debug, Error, Display)]
#[allow(missing_docs)] // FIXME
#[non_exhaustive]
pub(crate) enum ConfigurationError {
    /// could not read secret from file: {0}
    CannotReadSecretFromFile(std::io::Error),
    /// could not read secret from environment variable: {0}
    CannotReadSecretFromEnv(std::env::VarError),
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
}

/// The configuration for the router.
///
/// Can be created through `serde::Deserialize` from various formats,
/// or inline in Rust code with `serde_json::json!` and `serde_json::from_value`.
#[derive(Clone, Derivative, Deserialize, Serialize, JsonSchema, Default)]
#[derivative(Debug)]
pub struct Configuration {
    /// Configuration options pertaining to the http server component.
    #[serde(default)]
    pub(crate) server: Server,

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

const APOLLO_PLUGIN_PREFIX: &str = "apollo.";
const TELEMETRY_KEY: &str = "telemetry";

fn default_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:4000").unwrap().into()
}

#[buildstructor::buildstructor]
impl Configuration {
    #[builder]
    pub(crate) fn new(
        server: Option<Server>,
        cors: Option<Cors>,
        plugins: Map<String, Value>,
        apollo_plugins: Map<String, Value>,
    ) -> Self {
        Self {
            server: server.unwrap_or_default(),
            cors: cors.unwrap_or_default(),
            plugins: UserPlugins {
                plugins: Some(plugins),
            },
            apollo_plugins: ApolloPlugins {
                plugins: apollo_plugins,
            },
        }
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

/// Parse configuration from a string in YAML syntax
impl FromStr for Configuration {
    type Err = serde_yaml::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_yaml::from_str(s)
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

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Server {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:4000
    #[serde(default = "default_listen")]
    pub(crate) listen: ListenAddr,

    /// introspection queries
    /// enabled by default
    #[serde(default = "default_introspection")]
    pub(crate) introspection: bool,

    /// display landing page
    /// enabled by default
    #[serde(default = "default_landing_page")]
    pub(crate) landing_page: bool,

    /// The HTTP path on which GraphQL requests will be served.
    /// default: "/"
    #[serde(default = "default_graphql_path")]
    pub(crate) graphql_path: String,

    /// healthCheck path
    /// default: "/.well-known/apollo/server-health"
    #[serde(default = "default_health_check_path")]
    pub(crate) health_check_path: String,

    /// Preview @defer directive support
    /// default: true
    #[serde(default = "default_defer_support")]
    pub(crate) preview_defer_support: bool,

    /// Experimental limitation of query depth
    /// default: 4096
    #[serde(default = "default_parser_recursion_limit")]
    pub(crate) experimental_parser_recursion_limit: usize,
}

#[buildstructor::buildstructor]
impl Server {
    #[builder]
    #[allow(clippy::too_many_arguments)] // Used through a builder, not directly
    pub(crate) fn new(
        listen: Option<ListenAddr>,
        introspection: Option<bool>,
        landing_page: Option<bool>,
        graphql_path: Option<String>,
        health_check_path: Option<String>,
        defer_support: Option<bool>,
        parser_recursion_limit: Option<usize>,
    ) -> Self {
        Self {
            listen: listen.unwrap_or_else(default_listen),
            introspection: introspection.unwrap_or_else(default_introspection),
            landing_page: landing_page.unwrap_or_else(default_landing_page),
            graphql_path: graphql_path.unwrap_or_else(default_graphql_path),
            health_check_path: health_check_path.unwrap_or_else(default_health_check_path),
            preview_defer_support: defer_support.unwrap_or_else(default_defer_support),
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
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cors {
    /// Set to true to allow any origin.
    ///
    /// Defaults to false
    /// Having this set to true is the only way to allow Origin: null.
    #[serde(default)]
    pub(crate) allow_any_origin: bool,

    /// Set to true to add the `Access-Control-Allow-Credentials` header.
    #[serde(default)]
    pub(crate) allow_credentials: bool,

    /// The headers to allow.
    ///
    /// If this value is not set, the router will mirror client's `Access-Control-Request-Headers`.
    ///
    /// Note that if you set headers here,
    /// you also want to have a look at your `CSRF` plugins configuration,
    /// and make sure you either:
    /// - accept `x-apollo-operation-name` AND / OR `apollo-require-preflight`
    /// - defined `csrf` required headers in your yml configuration, as shown in the
    /// `examples/cors-and-csrf/custom-headers.router.yaml` files.
    #[serde(default)]
    pub(crate) allow_headers: Vec<String>,

    /// Which response headers should be made available to scripts running in the browser,
    /// in response to a cross-origin request.
    #[serde(default)]
    pub(crate) expose_headers: Option<Vec<String>>,

    /// The origin(s) to allow requests from.
    /// Defaults to `https://studio.apollographql.com/` for Apollo Studio.
    #[serde(default = "default_origins")]
    pub(crate) origins: Vec<String>,

    /// `Regex`es you want to match the origins against to determine if they're allowed.
    /// Defaults to an empty list.
    /// Note that `origins` will be evaluated before `match_origins`
    #[serde(default)]
    pub(crate) match_origins: Option<Vec<String>>,

    /// Allowed request methods. Defaults to GET, POST, OPTIONS.
    #[serde(default = "default_cors_methods")]
    pub(crate) methods: Vec<String>,
}

impl Default for Cors {
    fn default() -> Self {
        Self {
            origins: default_origins(),
            methods: default_cors_methods(),
            allow_any_origin: Default::default(),
            allow_credentials: Default::default(),
            allow_headers: Default::default(),
            expose_headers: Default::default(),
            match_origins: Default::default(),
        }
    }
}

fn default_origins() -> Vec<String> {
    vec!["https://studio.apollographql.com".into()]
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

fn default_graphql_path() -> String {
    String::from("/")
}

fn default_health_check_path() -> String {
    String::from("/.well-known/apollo/server-health")
}

fn default_defer_support() -> bool {
    true
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

#[cfg(test)]
#[buildstructor::buildstructor]
impl Cors {
    #[builder]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        allow_any_origin: Option<bool>,
        allow_credentials: Option<bool>,
        allow_headers: Option<Vec<String>>,
        expose_headers: Option<Vec<String>>,
        origins: Option<Vec<String>>,
        match_origins: Option<Vec<String>>,
        methods: Option<Vec<String>>,
    ) -> Self {
        Self {
            expose_headers,
            match_origins,
            origins: origins.unwrap_or_else(default_origins),
            methods: methods.unwrap_or_else(default_cors_methods),
            allow_any_origin: allow_any_origin.unwrap_or_default(),
            allow_credentials: allow_credentials.unwrap_or_default(),
            allow_headers: allow_headers.unwrap_or_default(),
        }
    }
}

impl Cors {
    pub(crate) fn into_layer(self) -> Result<CorsLayer, String> {
        // Ensure configuration is valid before creating CorsLayer

        self.ensure_usable_cors_rules()?;

        let allow_headers = if self.allow_headers.is_empty() {
            cors::AllowHeaders::mirror_request()
        } else {
            cors::AllowHeaders::list(self.allow_headers.iter().filter_map(|header| {
                header
                    .parse()
                    .map_err(|_| tracing::error!("header name '{header}' is not valid"))
                    .ok()
            }))
        };
        let cors = CorsLayer::new()
            .vary([])
            .allow_credentials(self.allow_credentials)
            .allow_headers(allow_headers)
            .expose_headers(cors::ExposeHeaders::list(
                self.expose_headers
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|header| {
                        header
                            .parse()
                            .map_err(|_| tracing::error!("header name '{header}' is not valid"))
                            .ok()
                    }),
            ))
            .allow_methods(cors::AllowMethods::list(self.methods.iter().filter_map(
                |method| {
                    method
                        .parse()
                        .map_err(|_| tracing::error!("method '{method}' is not valid"))
                        .ok()
                },
            )));

        if self.allow_any_origin {
            Ok(cors.allow_origin(cors::Any))
        } else if let Some(match_origins) = self.match_origins {
            let regexes = match_origins
                .into_iter()
                .filter_map(|regex| {
                    Regex::from_str(regex.as_str())
                        .map_err(|_| tracing::error!("origin regex '{regex}' is not valid"))
                        .ok()
                })
                .collect::<Vec<_>>();

            Ok(cors.allow_origin(cors::AllowOrigin::predicate(
                move |origin: &HeaderValue, _: &Parts| {
                    origin
                        .to_str()
                        .map(|o| {
                            self.origins.iter().any(|origin| origin.as_str() == o)
                                || regexes.iter().any(|regex| regex.is_match(o))
                        })
                        .unwrap_or_default()
                },
            )))
        } else {
            Ok(cors.allow_origin(cors::AllowOrigin::list(
                self.origins.into_iter().filter_map(|origin| {
                    origin
                        .parse()
                        .map_err(|_| tracing::error!("origin '{origin}' is not valid"))
                        .ok()
                }),
            )))
        }
    }

    // This is cribbed from the similarly named function in tower-http. The version there
    // asserts that CORS rules are useable, which results in a panic if they aren't. We
    // don't want the router to panic in such cases, so this function returns an error
    // with a message describing what the problem is.
    fn ensure_usable_cors_rules(&self) -> Result<(), &'static str> {
        if self.allow_credentials {
            if self.allow_headers.iter().any(|x| x == "*") {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                        with `Access-Control-Allow-Headers: *`");
            }

            if self.methods.iter().any(|x| x == "*") {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `Access-Control-Allow-Methods: *`");
            }

            if self.origins.iter().any(|x| x == "*") {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `Access-Control-Allow-Origin: *`");
            }

            if self.allow_any_origin {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `Access-Control-Allow-Origin: *`");
            }

            if let Some(headers) = &self.expose_headers {
                if headers.iter().any(|x| x == "*") {
                    return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                        with `Access-Control-Expose-Headers: *`");
                }
            }
        }
        Ok(())
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
/// 3. Validate the yaml against the json schema.
/// 4. If there were errors then try and parse using a custom parser that retains line and column number info.
/// 5. Convert the json paths from the error messages into nice error snippets.
///
/// There may still be serde validation issues later.
///
pub(crate) fn validate_configuration(raw_yaml: &str) -> Result<Configuration, ConfigurationError> {
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
    let expanded_yaml = expand_env_variables(yaml);
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

    // Custom validations
    if !config.server.graphql_path.starts_with('/') {
        return Err(ConfigurationError::InvalidConfiguration {
            message: "invalid 'server.graphql_path' configuration",
            error: format!(
                "'{}' is invalid, it must be an absolute path and start with '/', you should try with '/{}'",
                config.server.graphql_path,
                config.server.graphql_path
            ),
        });
    }
    if config.server.graphql_path.ends_with('*') && !config.server.graphql_path.ends_with("/*") {
        return Err(ConfigurationError::InvalidConfiguration {
            message: "invalid 'server.graphql_path' configuration",
            error: format!(
                "'{}' is invalid, you can only set a wildcard after a '/'",
                config.server.graphql_path
            ),
        });
    }
    if config.server.graphql_path.contains("/*/") {
        return Err(
                ConfigurationError::InvalidConfiguration {
                    message: "invalid 'server.graphql_path' configuration",
                    error: format!(
                        "'{}' is invalid, if you need to set a path like '/*/graphql' then specify it as a path parameter with a name, for example '/:my_project_key/graphql'",
                        config.server.graphql_path
                    ),
                },
            );
    }

    Ok(config)
}

fn expand_env_variables(configuration: &serde_json::Value) -> serde_json::Value {
    let mut configuration = configuration.clone();
    visit(&mut configuration);
    configuration
}

fn visit(value: &mut Value) {
    let mut expanded: Option<String> = None;
    match value {
        Value::String(value) => {
            let new_value = envmnt::expand(
                value,
                Some(
                    ExpandOptions::new()
                        .clone_with_expansion_type(ExpansionType::UnixBracketsWithDefaults),
                ),
            );

            if &new_value != value {
                expanded = Some(new_value);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(visit),
        Value::Object(o) => o.iter_mut().for_each(|(_, v)| visit(v)),
        _ => {}
    }
    // The expansion may have resulted in a primitive, reparse and replace
    if let Some(expanded) = expanded {
        *value = coerce(&expanded)
    }
}

fn coerce(expanded: &str) -> Value {
    match serde_yaml::from_str(expanded) {
        Ok(Value::Bool(b)) => Value::Bool(b),
        Ok(Value::Number(n)) => Value::Number(n),
        Ok(Value::Null) => Value::Null,
        _ => Value::String(expanded.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use http::Uri;
    #[cfg(unix)]
    use insta::assert_json_snapshot;
    use regex::Regex;
    #[cfg(unix)]
    use schemars::gen::SchemaSettings;
    use walkdir::DirEntry;
    use walkdir::WalkDir;

    use super::*;
    use crate::error::SchemaError;

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
        let schema = r#"
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
        }
        "#;
        let schema = crate::Schema::parse(schema, &Default::default()).unwrap();

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
        }"#;
        let schema_error = crate::Schema::parse(schema_error, &Default::default())
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
            !cors.allow_any_origin,
            "Allow any origin should be disabled by default"
        );
        assert!(cors.allow_headers.is_empty());

        assert!(
            cors.match_origins.is_none(),
            "No origin regex list should be present by default"
        );
    }

    #[test]
    fn bad_graphql_path_configuration_without_slash() {
        let error = validate_configuration(
            r#"
server:
  graphql_path: test
  "#,
        )
        .expect_err("should have resulted in an error");
        assert_eq!(error.to_string(), String::from("invalid 'server.graphql_path' configuration: 'test' is invalid, it must be an absolute path and start with '/', you should try with '/test'"));
    }

    #[test]
    fn bad_graphql_path_configuration_with_wildcard_as_prefix() {
        let error = validate_configuration(
            r#"
server:
  graphql_path: /*/test
  "#,
        )
        .expect_err("should have resulted in an error");
        assert_eq!(error.to_string(), String::from("invalid 'server.graphql_path' configuration: '/*/test' is invalid, if you need to set a path like '/*/graphql' then specify it as a path parameter with a name, for example '/:my_project_key/graphql'"));
    }

    #[test]
    fn unknown_fields() {
        let error = validate_configuration(
            r#"
server:
  graphql_path: /
subgraphs:
  account: true
  "#,
        )
        .expect_err("should have resulted in an error");
        assert_eq!(error.to_string(), String::from("unknown fields: additional properties are not allowed ('subgraphs' was/were unexpected)"));
    }

    #[test]
    fn unknown_fields_at_root() {
        let error = validate_configuration(
            r#"
unknown:
  foo: true
  "#,
        )
        .expect_err("should have resulted in an error");
        assert_eq!(error.to_string(), String::from("unknown fields: additional properties are not allowed ('unknown' was/were unexpected)"));
    }

    #[test]
    fn empty_config() {
        validate_configuration(
            r#"
  "#,
        )
        .expect("should have been ok with an empty config");
    }

    #[test]
    fn bad_graphql_path_configuration_with_bad_ending_wildcard() {
        let error = validate_configuration(
            r#"
server:
  graphql_path: /test*
  "#,
        )
        .expect_err("should have resulted in an error");
        assert_eq!(error.to_string(), String::from("invalid 'server.graphql_path' configuration: '/test*' is invalid, you can only set a wildcard after a '/'"));
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
    fn line_precise_config_errors_with_errors_after_first_field() {
        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
  bad: "donotwork"
  another_one: true
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn line_precise_config_errors_bad_type() {
        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: true
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn line_precise_config_errors_with_inline_sequence() {
        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers: [ Content-Type, 5 ]
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn line_precise_config_errors_with_sequence() {
        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers:
    - Content-Type
    - 5
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn it_does_not_allow_invalid_cors_headers() {
        let cfg = validate_configuration(
            r#"
cors:
  allow_credentials: true
  allow_headers: [ "*" ]
        "#,
        )
        .expect("should not have resulted in an error");
        let error = cfg
            .cors
            .into_layer()
            .expect_err("should have resulted in an error");
        assert_eq!(error, "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Headers: *`");
    }

    #[test]
    fn it_does_not_allow_invalid_cors_methods() {
        let cfg = validate_configuration(
            r#"
cors:
  allow_credentials: true
  methods: [ GET, "*" ]
        "#,
        )
        .expect("should not have resulted in an error");
        let error = cfg
            .cors
            .into_layer()
            .expect_err("should have resulted in an error");
        assert_eq!(error, "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Methods: *`");
    }

    #[test]
    fn it_does_not_allow_invalid_cors_origins() {
        let cfg = validate_configuration(
            r#"
cors:
  allow_credentials: true
  allow_any_origin: true
        "#,
        )
        .expect("should not have resulted in an error");
        let error = cfg
            .cors
            .into_layer()
            .expect_err("should have resulted in an error");
        assert_eq!(error, "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Origin: *`");
    }

    #[test]
    fn validate_project_config_files() {
        #[cfg(not(unix))]
        let filename_matcher = Regex::from_str("((.+[.])?router\\.yaml)|(.+\\.mdx)").unwrap();
        #[cfg(unix)]
        let filename_matcher =
            Regex::from_str("((.+[.])?router(_unix)?\\.yaml)|(.+\\.mdx)").unwrap();
        #[cfg(not(unix))]
        let embedded_yaml_matcher =
            Regex::from_str(r#"(?ms)```yaml title="router.yaml"(.+?)```"#).unwrap();
        #[cfg(unix)]
        let embedded_yaml_matcher =
            Regex::from_str(r#"(?ms)```yaml title="router(_unix)?.yaml"(.+?)```"#).unwrap();

        fn it(path: &str) -> impl Iterator<Item = DirEntry> {
            WalkDir::new(path).into_iter().filter_map(|e| e.ok())
        }

        for entry in it(".").chain(it("../examples")).chain(it("../docs")) {
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
                    #[cfg(unix)]
                    let index = 2usize;
                    #[cfg(not(unix))]
                    let index = 1usize;
                    // Extract yaml from docs
                    embedded_yaml_matcher
                        .captures_iter(&config)
                        .map(|i| i.get(index).unwrap().as_str().into())
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

    #[test]
    fn it_does_not_leak_env_variable_values() {
        std::env::set_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5");
        let error = validate_configuration(
            r#"
server:
  introspection: ${TEST_CONFIG_NUMERIC_ENV_UNIQUE:true}
        "#,
        )
        .expect_err("Must have an error because we expect a boolean");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn line_precise_config_errors_with_inline_sequence_env_expansion() {
        std::env::set_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5");
        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers: [ Content-Type, "${TEST_CONFIG_NUMERIC_ENV_UNIQUE}" ]
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn line_precise_config_errors_with_sequence_env_expansion() {
        std::env::set_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5");

        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers:
    - Content-Type
    - "${TEST_CONFIG_NUMERIC_ENV_UNIQUE:true}"
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }

    #[test]
    fn line_precise_config_errors_with_errors_after_first_field_env_expansion() {
        let error = validate_configuration(
            r#"
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
  ${TEST_CONFIG_NUMERIC_ENV_UNIQUE:true}: 5
  another_one: ${TEST_CONFIG_NUMERIC_ENV_UNIQUE:true}
        "#,
        )
        .expect_err("should have resulted in an error");
        insta::assert_snapshot!(error.to_string());
    }
}
