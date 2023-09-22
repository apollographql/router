//! Logic for loading configuration in to an object model
pub(crate) mod cors;
pub(crate) mod expansion;
mod experimental;
pub(crate) mod metrics;
mod persisted_queries;
mod schema;
pub(crate) mod subgraph;
#[cfg(test)]
mod tests;
mod upgrade;
mod yaml;

use std::fmt;
use std::io;
use std::io::BufReader;
use std::iter;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use derivative::Derivative;
use displaydoc::Display;
use itertools::Itertools;
use once_cell::sync::Lazy;
pub(crate) use persisted_queries::PersistedQueries;
#[cfg(test)]
pub(crate) use persisted_queries::PersistedQueriesSafelist;
use regex::Regex;
use rustls::Certificate;
use rustls::PrivateKey;
use rustls::ServerConfig;
use rustls_pemfile::certs;
use rustls_pemfile::read_one;
use rustls_pemfile::Item;
use schemars::gen::SchemaGenerator;
use schemars::schema::ObjectValidation;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use thiserror::Error;

use self::cors::Cors;
use self::expansion::Expansion;
pub(crate) use self::experimental::Discussed;
pub(crate) use self::schema::generate_config_schema;
pub(crate) use self::schema::generate_upgrade;
use self::subgraph::SubgraphConfiguration;
use crate::cache::DEFAULT_CACHE_CAPACITY;
use crate::configuration::schema::Mode;
use crate::graphql;
use crate::notification::Notify;
#[cfg(not(test))]
use crate::notification::RouterBroadcasts;
use crate::plugin::plugins;
#[cfg(not(test))]
use crate::plugins::subscription::SubscriptionConfig;
#[cfg(not(test))]
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
#[cfg(not(test))]
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN_NAME;
use crate::uplink::UplinkConfig;
use crate::ApolloRouterError;

// TODO: Talk it through with the teams
#[cfg(not(test))]
static HEARTBEAT_TIMEOUT_DURATION_SECONDS: u64 = 15;

static SUPERGRAPH_ENDPOINT_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<first_path>.*/)(?P<sub_path>.+)\*$")
        .expect("this regex to check the path is valid")
});

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

    /// could not migrate configuration: {error}.
    MigrationFailure { error: String },

    /// could not load certificate authorities: {error}
    CertificateAuthorities { error: String },
}

/// The configuration for the router.
///
/// Can be created through `serde::Deserialize` from various formats,
/// or inline in Rust code with `serde_json::json!` and `serde_json::from_value`.
#[derive(Clone, Derivative, Serialize, JsonSchema)]
#[derivative(Debug)]
// We can't put a global #[serde(default)] here because of the Default implementation using `from_str` which use deserialize
pub struct Configuration {
    /// The raw configuration string.
    #[serde(skip)]
    pub(crate) validated_yaml: Option<Value>,

    /// Health check configuration
    #[serde(default)]
    pub(crate) health_check: HealthCheck,

    /// Sandbox configuration
    #[serde(default)]
    pub(crate) sandbox: Sandbox,

    /// Homepage configuration
    #[serde(default)]
    pub(crate) homepage: Homepage,

    /// Configuration for the supergraph
    #[serde(default)]
    pub(crate) supergraph: Supergraph,

    /// Cross origin request headers.
    #[serde(default)]
    pub(crate) cors: Cors,

    #[serde(default)]
    pub(crate) tls: Tls,

    /// Configures automatic persisted queries
    #[serde(default)]
    pub(crate) apq: Apq,

    // NOTE: when renaming this to move out of preview, also update paths
    // in `uplink/license.rs`.
    /// Configures managed persisted queries
    #[serde(default)]
    pub preview_persisted_queries: PersistedQueries,

    /// Configuration for operation limits, parser limits, HTTP limits, etc.
    #[serde(default)]
    pub(crate) limits: Limits,

    /// Configuration for chaos testing, trying to reproduce bugs that require uncommon conditions.
    /// You probably don’t want this in production!
    #[serde(default)]
    pub(crate) experimental_chaos: Chaos,

    /// Set the GraphQL validation implementation to use.
    #[serde(default)]
    pub(crate) experimental_graphql_validation_mode: GraphQLValidationMode,

    /// Plugin configuration
    #[serde(default)]
    pub(crate) plugins: UserPlugins,

    /// Built-in plugin configuration. Built in plugins are pushed to the top level of config.
    #[serde(default)]
    #[serde(flatten)]
    pub(crate) apollo_plugins: ApolloPlugins,

    /// Uplink configuration.
    #[serde(skip)]
    pub uplink: Option<UplinkConfig>,

    #[serde(default, skip_serializing, skip_deserializing)]
    pub(crate) notify: Notify<String, graphql::Response>,
}

impl PartialEq for Configuration {
    fn eq(&self, other: &Self) -> bool {
        self.validated_yaml == other.validated_yaml
    }
}

/// GraphQL validation modes.
#[derive(Clone, PartialEq, Eq, Default, Derivative, Serialize, Deserialize, JsonSchema)]
#[derivative(Debug)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GraphQLValidationMode {
    /// Use the new Rust-based implementation.
    New,
    /// Use the old JavaScript-based implementation.
    #[default]
    Legacy,
    /// Use Rust-based and Javascript-based implementations side by side, logging warnings if the
    /// implementations disagree.
    Both,
}

impl<'de> serde::Deserialize<'de> for Configuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // This intermediate structure will allow us to deserialize a Configuration
        // yet still exercise the Configuration validation function
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct AdHocConfiguration {
            health_check: HealthCheck,
            sandbox: Sandbox,
            homepage: Homepage,
            supergraph: Supergraph,
            cors: Cors,
            plugins: UserPlugins,
            #[serde(flatten)]
            apollo_plugins: ApolloPlugins,
            tls: Tls,
            apq: Apq,
            preview_persisted_queries: PersistedQueries,
            #[serde(skip)]
            uplink: UplinkConfig,
            limits: Limits,
            experimental_chaos: Chaos,
            experimental_graphql_validation_mode: GraphQLValidationMode,
        }
        let ad_hoc: AdHocConfiguration = serde::Deserialize::deserialize(deserializer)?;

        Configuration::builder()
            .health_check(ad_hoc.health_check)
            .sandbox(ad_hoc.sandbox)
            .homepage(ad_hoc.homepage)
            .supergraph(ad_hoc.supergraph)
            .cors(ad_hoc.cors)
            .plugins(ad_hoc.plugins.plugins.unwrap_or_default())
            .apollo_plugins(ad_hoc.apollo_plugins.plugins)
            .tls(ad_hoc.tls)
            .apq(ad_hoc.apq)
            .persisted_query(ad_hoc.preview_persisted_queries)
            .operation_limits(ad_hoc.limits)
            .chaos(ad_hoc.experimental_chaos)
            .uplink(ad_hoc.uplink)
            .graphql_validation_mode(ad_hoc.experimental_graphql_validation_mode)
            .build()
            .map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

pub(crate) const APOLLO_PLUGIN_PREFIX: &str = "apollo.";

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
        supergraph: Option<Supergraph>,
        health_check: Option<HealthCheck>,
        sandbox: Option<Sandbox>,
        homepage: Option<Homepage>,
        cors: Option<Cors>,
        plugins: Map<String, Value>,
        apollo_plugins: Map<String, Value>,
        tls: Option<Tls>,
        notify: Option<Notify<String, graphql::Response>>,
        apq: Option<Apq>,
        persisted_query: Option<PersistedQueries>,
        operation_limits: Option<Limits>,
        chaos: Option<Chaos>,
        uplink: Option<UplinkConfig>,
        graphql_validation_mode: Option<GraphQLValidationMode>,
    ) -> Result<Self, ConfigurationError> {
        #[cfg(not(test))]
        let notify_queue_cap = match apollo_plugins.get(APOLLO_SUBSCRIPTION_PLUGIN_NAME) {
            Some(plugin_conf) => {
                let conf = serde_json::from_value::<SubscriptionConfig>(plugin_conf.clone())
                    .map_err(|err| ConfigurationError::PluginConfiguration {
                        plugin: APOLLO_SUBSCRIPTION_PLUGIN.to_string(),
                        error: format!("{err:?}"),
                    })?;
                conf.queue_capacity
            }
            None => None,
        };

        let conf = Self {
            validated_yaml: Default::default(),
            supergraph: supergraph.unwrap_or_default(),
            health_check: health_check.unwrap_or_default(),
            sandbox: sandbox.unwrap_or_default(),
            homepage: homepage.unwrap_or_default(),
            cors: cors.unwrap_or_default(),
            apq: apq.unwrap_or_default(),
            preview_persisted_queries: persisted_query.unwrap_or_default(),
            limits: operation_limits.unwrap_or_default(),
            experimental_chaos: chaos.unwrap_or_default(),
            experimental_graphql_validation_mode: graphql_validation_mode.unwrap_or_default(),
            plugins: UserPlugins {
                plugins: Some(plugins),
            },
            apollo_plugins: ApolloPlugins {
                plugins: apollo_plugins,
            },
            tls: tls.unwrap_or_default(),
            uplink,
            #[cfg(test)]
            notify: notify.unwrap_or_default(),
            #[cfg(not(test))]
            notify: notify.map(|n| n.set_queue_size(notify_queue_cap))
                .unwrap_or_else(|| Notify::builder().and_queue_size(notify_queue_cap).ttl(Duration::from_secs(HEARTBEAT_TIMEOUT_DURATION_SECONDS)).router_broadcasts(Arc::new(RouterBroadcasts::new())).heartbeat_error_message(graphql::Response::builder().errors(vec![graphql::Error::builder().message("the connection has been closed because it hasn't heartbeat for a while").extension_code("SUBSCRIPTION_HEARTBEAT_ERROR").build()]).build()).build()),
        };

        conf.validate()
    }
}

impl Default for Configuration {
    fn default() -> Self {
        // We want to trigger all defaulting logic so don't use the raw builder.
        Configuration::from_str("").expect("default configuration must be valid")
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Configuration {
    #[builder]
    pub(crate) fn fake_new(
        supergraph: Option<Supergraph>,
        health_check: Option<HealthCheck>,
        sandbox: Option<Sandbox>,
        homepage: Option<Homepage>,
        cors: Option<Cors>,
        plugins: Map<String, Value>,
        apollo_plugins: Map<String, Value>,
        tls: Option<Tls>,
        notify: Option<Notify<String, graphql::Response>>,
        apq: Option<Apq>,
        persisted_query: Option<PersistedQueries>,
        operation_limits: Option<Limits>,
        chaos: Option<Chaos>,
        uplink: Option<UplinkConfig>,
        graphql_validation_mode: Option<GraphQLValidationMode>,
    ) -> Result<Self, ConfigurationError> {
        let configuration = Self {
            validated_yaml: Default::default(),
            supergraph: supergraph.unwrap_or_else(|| Supergraph::fake_builder().build()),
            health_check: health_check.unwrap_or_else(|| HealthCheck::fake_builder().build()),
            sandbox: sandbox.unwrap_or_else(|| Sandbox::fake_builder().build()),
            homepage: homepage.unwrap_or_else(|| Homepage::fake_builder().build()),
            cors: cors.unwrap_or_default(),
            limits: operation_limits.unwrap_or_default(),
            experimental_chaos: chaos.unwrap_or_default(),
            experimental_graphql_validation_mode: graphql_validation_mode.unwrap_or_default(),
            plugins: UserPlugins {
                plugins: Some(plugins),
            },
            apollo_plugins: ApolloPlugins {
                plugins: apollo_plugins,
            },
            tls: tls.unwrap_or_default(),
            notify: notify.unwrap_or_default(),
            apq: apq.unwrap_or_default(),
            preview_persisted_queries: persisted_query.unwrap_or_default(),
            uplink,
        };

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
        if self.supergraph.path.ends_with('*')
            && !self.supergraph.path.ends_with("/*")
            && !SUPERGRAPH_ENDPOINT_REGEX.is_match(&self.supergraph.path)
        {
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

        // PQs.
        if self.preview_persisted_queries.enabled {
            if self.preview_persisted_queries.safelist.enabled && self.apq.enabled {
                return Err(ConfigurationError::InvalidConfiguration {
                    message: "apqs must be disabled to enable safelisting",
                    error: "either set preview_persisted_queries.safelist.enabled: false or apq.enabled: false in your router yaml configuration".into()
                });
            } else if !self.preview_persisted_queries.safelist.enabled
                && self.preview_persisted_queries.safelist.require_id
            {
                return Err(ConfigurationError::InvalidConfiguration {
                    message: "safelist must be enabled to require IDs",
                    error: "either set preview_persisted_queries.safelist.enabled: true or preview_persisted_queries.safelist.require_id: false in your router yaml configuration".into()
                });
            }
        } else {
            // If the feature isn't enabled, sub-features shouldn't be.
            if self.preview_persisted_queries.safelist.enabled {
                return Err(ConfigurationError::InvalidConfiguration {
                    message: "persisted queries must be enabled to enable safelisting",
                    error: "either set preview_persisted_queries.safelist.enabled: false or preview_persisted_queries.enabled: true in your router yaml configuration".into()
                });
            } else if self.preview_persisted_queries.log_unknown {
                return Err(ConfigurationError::InvalidConfiguration {
                    message: "persisted queries must be enabled to enable logging unknown operations",
                    error: "either set preview_persisted_queries.log_unknown: false or preview_persisted_queries.enabled: true in your router yaml configuration".into()
                });
            }
        }

        Ok(self)
    }
}

/// Parse configuration from a string in YAML syntax
impl FromStr for Configuration {
    type Err = ConfigurationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        schema::validate_yaml_configuration(s, Expansion::default()?, Mode::Upgrade)?.validate()
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
            .sorted_by_key(|factory| factory.name.clone())
            .filter(|factory| factory.name.starts_with(APOLLO_PLUGIN_PREFIX))
            .map(|factory| {
                (
                    factory.name[APOLLO_PLUGIN_PREFIX.len()..].to_string(),
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
            .sorted_by_key(|factory| factory.name.clone())
            .filter(|factory| !factory.name.starts_with(APOLLO_PLUGIN_PREFIX))
            .map(|factory| (factory.name.to_string(), factory.create_schema(gen)))
            .collect::<schemars::Map<String, Schema>>();
        gen_schema(plugins)
    }
}

/// Configuration options pertaining to the supergraph server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Supergraph {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:4000
    pub(crate) listen: ListenAddr,

    /// The HTTP path on which GraphQL requests will be served.
    /// default: "/"
    pub(crate) path: String,

    /// Enable introspection
    /// Default: false
    pub(crate) introspection: bool,

    /// Enable reuse of query fragments
    /// Default: depends on the federation version
    #[serde(rename = "experimental_reuse_query_fragments")]
    pub(crate) reuse_query_fragments: Option<bool>,

    /// Set to false to disable defer support
    pub(crate) defer_support: bool,

    /// Query planning options
    pub(crate) query_planning: QueryPlanning,
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
        defer_support: Option<bool>,
        query_planning: Option<QueryPlanning>,
        reuse_query_fragments: Option<bool>,
    ) -> Self {
        Self {
            listen: listen.unwrap_or_else(default_graphql_listen),
            path: path.unwrap_or_else(default_graphql_path),
            introspection: introspection.unwrap_or_else(default_graphql_introspection),
            defer_support: defer_support.unwrap_or_else(default_defer_support),
            query_planning: query_planning.unwrap_or_default(),
            reuse_query_fragments,
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
        defer_support: Option<bool>,
        query_planning: Option<QueryPlanning>,
        reuse_query_fragments: Option<bool>,
    ) -> Self {
        Self {
            listen: listen.unwrap_or_else(test_listen),
            path: path.unwrap_or_else(default_graphql_path),
            introspection: introspection.unwrap_or_else(default_graphql_introspection),
            defer_support: defer_support.unwrap_or_else(default_defer_support),
            query_planning: query_planning.unwrap_or_default(),
            reuse_query_fragments,
        }
    }
}

impl Default for Supergraph {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl Supergraph {
    /// To sanitize the path for axum router
    pub(crate) fn sanitized_path(&self) -> String {
        let mut path = self.path.clone();
        if self.path.ends_with("/*") {
            // Needed for axum (check the axum docs for more information about wildcards https://docs.rs/axum/latest/axum/struct.Router.html#wildcards)
            path = format!("{}router_extra_path", self.path);
        } else if SUPERGRAPH_ENDPOINT_REGEX.is_match(&self.path) {
            let new_path = SUPERGRAPH_ENDPOINT_REGEX
                .replace(&self.path, "${first_path}${sub_path}:supergraph_route");
            path = new_path.to_string();
        }

        path
    }
}

/// Configuration for operation limits, parser limits, HTTP limits, etc.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Limits {
    /// If set, requests with operations deeper than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Counts depth of an operation, looking at its selection sets,
    /// including fields in fragments and inline fragments. The following
    /// example has a depth of 3.
    ///
    /// ```graphql
    /// query getProduct {
    ///   book { # 1
    ///     ...bookDetails
    ///   }
    /// }
    ///
    /// fragment bookDetails on Book {
    ///   details { # 2
    ///     ... on ProductDetailsBook {
    ///       country # 3
    ///     }
    ///   }
    /// }
    /// ```
    pub(crate) max_depth: Option<u32>,

    /// If set, requests with operations higher than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Height is based on simple merging of fields using the same name or alias,
    /// but only within the same selection set.
    /// For example `name` here is only counted once and the query has height 3, not 4:
    ///
    /// ```graphql
    /// query {
    ///     name { first }
    ///     name { last }
    /// }
    /// ```
    ///
    /// This may change in a future version of Apollo Router to do
    /// [full field merging across fragments][merging] instead.
    ///
    /// [merging]: https://spec.graphql.org/October2021/#sec-Field-Selection-Merging]
    pub(crate) max_height: Option<u32>,

    /// If set, requests with operations with more root fields than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_ROOT_FIELDS_LIMIT"}`
    ///
    /// This limit counts only the top level fields in a selection set,
    /// including fragments and inline fragments.
    pub(crate) max_root_fields: Option<u32>,

    /// If set, requests with operations with more aliases than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_ALIASES_LIMIT"}`
    pub(crate) max_aliases: Option<u32>,

    /// If set to true (which is the default is dev mode),
    /// requests that exceed a `max_*` limit are *not* rejected.
    /// Instead they are executed normally, and a warning is logged.
    pub(crate) warn_only: bool,

    /// Limit recursion in the GraphQL parser to protect against stack overflow.
    /// default: 4096
    pub(crate) parser_max_recursion: usize,

    /// Limit the number of tokens the GraphQL parser processes before aborting.
    pub(crate) parser_max_tokens: usize,

    /// Limit the size of incoming HTTP requests read from the network,
    /// to protect against running out of memory. Default: 2000000 (2 MB)
    pub(crate) experimental_http_max_request_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // These limits are opt-in
            max_depth: None,
            max_height: None,
            max_root_fields: None,
            max_aliases: None,
            warn_only: false,
            experimental_http_max_request_bytes: 2_000_000,
            parser_max_tokens: 15_000,

            // This is `apollo-parser`’s default, which protects against stack overflow
            // but is still very high for "reasonable" queries.
            // https://docs.rs/apollo-parser/0.2.8/src/apollo_parser/parser/mod.rs.html#368
            parser_max_recursion: 4096,
        }
    }
}

/// Router level (APQ) configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Router {
    #[serde(default)]
    pub(crate) cache: Cache,
}

/// Automatic Persisted Queries (APQ) configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Apq {
    /// Activates Automatic Persisted Queries (enabled by default)
    #[serde(default = "default_apq")]
    pub(crate) enabled: bool,

    #[serde(default)]
    pub(crate) router: Router,

    #[serde(default)]
    pub(crate) subgraph: SubgraphConfiguration<SubgraphApq>,
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Apq {
    #[builder]
    pub(crate) fn fake_new(enabled: Option<bool>) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_apq),
            ..Default::default()
        }
    }
}

/// Subgraph level Automatic Persisted Queries (APQ) configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubgraphApq {
    /// Enable
    #[serde(default = "default_subgraph_apq")]
    pub(crate) enabled: bool,
}

fn default_subgraph_apq() -> bool {
    false
}

fn default_apq() -> bool {
    true
}

impl Default for Apq {
    fn default() -> Self {
        Self {
            enabled: default_apq(),
            router: Default::default(),
            subgraph: Default::default(),
        }
    }
}

/// Query planning cache configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct QueryPlanning {
    /// Cache configuration
    pub(crate) experimental_cache: Cache,
    /// Warms up the cache on reloads by running the query plan over
    /// a list of the most used queries (from the in memory cache)
    /// Configures the number of queries warmed up. Defaults to 1/3 of
    /// the in memory cache
    #[serde(default)]
    pub(crate) warmed_up_queries: Option<usize>,
}

/// Cache configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Cache {
    /// Configures the in memory cache (always active)
    pub(crate) in_memory: InMemoryCache,
    /// Configures and activates the Redis cache
    pub(crate) redis: Option<RedisCache>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// In memory cache configuration
pub(crate) struct InMemoryCache {
    /// Number of entries in the Least Recently Used cache
    pub(crate) limit: NonZeroUsize,
}

impl Default for InMemoryCache {
    fn default() -> Self {
        Self {
            limit: DEFAULT_CACHE_CAPACITY,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Redis cache configuration
pub(crate) struct RedisCache {
    /// List of URLs to the Redis cluster
    pub(crate) urls: Vec<url::Url>,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    /// Redis request timeout (default: 2ms)
    pub(crate) timeout: Option<Duration>,
}

/// TLS related configuration options.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Tls {
    /// TLS server configuration
    ///
    /// this will affect the GraphQL endpoint and any other endpoint targeting the same listen address
    pub(crate) supergraph: Option<TlsSupergraph>,
    pub(crate) subgraph: SubgraphConfiguration<TlsSubgraph>,
}

/// Configuration options pertaining to the supergraph server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsSupergraph {
    /// server certificate in PEM format
    #[serde(deserialize_with = "deserialize_certificate", skip_serializing)]
    #[schemars(with = "String")]
    pub(crate) certificate: Certificate,
    /// server key in PEM format
    #[serde(deserialize_with = "deserialize_key", skip_serializing)]
    #[schemars(with = "String")]
    pub(crate) key: PrivateKey,
    /// list of certificate authorities in PEM format
    #[serde(deserialize_with = "deserialize_certificate_chain", skip_serializing)]
    #[schemars(with = "String")]
    pub(crate) certificate_chain: Vec<Certificate>,
}

impl TlsSupergraph {
    pub(crate) fn tls_config(&self) -> Result<Arc<rustls::ServerConfig>, ApolloRouterError> {
        let mut certificates = vec![self.certificate.clone()];
        certificates.extend(self.certificate_chain.iter().cloned());

        let mut config = ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(certificates, self.key.clone())
            .map_err(ApolloRouterError::Rustls)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        Ok(Arc::new(config))
    }
}

fn deserialize_certificate<'de, D>(deserializer: D) -> Result<Certificate, D::Error>
where
    D: Deserializer<'de>,
{
    let data = String::deserialize(deserializer)?;

    load_certs(&data)
        .map_err(serde::de::Error::custom)
        .and_then(|mut certs| {
            if certs.len() > 1 {
                Err(serde::de::Error::custom("expected exactly one certificate"))
            } else {
                certs
                    .pop()
                    .ok_or(serde::de::Error::custom("expected exactly one certificate"))
            }
        })
}

fn deserialize_certificate_chain<'de, D>(deserializer: D) -> Result<Vec<Certificate>, D::Error>
where
    D: Deserializer<'de>,
{
    let data = String::deserialize(deserializer)?;

    load_certs(&data).map_err(serde::de::Error::custom)
}

fn deserialize_key<'de, D>(deserializer: D) -> Result<PrivateKey, D::Error>
where
    D: Deserializer<'de>,
{
    let data = String::deserialize(deserializer)?;

    load_key(&data).map_err(serde::de::Error::custom)
}

pub(crate) fn load_certs(data: &str) -> io::Result<Vec<Certificate>> {
    certs(&mut BufReader::new(data.as_bytes()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid cert"))
        .map(|mut certs| certs.drain(..).map(Certificate).collect())
}

pub(crate) fn load_key(data: &str) -> io::Result<PrivateKey> {
    let mut reader = BufReader::new(data.as_bytes());
    let mut key_iterator = iter::from_fn(|| read_one(&mut reader).transpose());

    let private_key = match key_iterator.next() {
        Some(Ok(Item::RSAKey(key))) => PrivateKey(key),
        Some(Ok(Item::PKCS8Key(key))) => PrivateKey(key),
        Some(Ok(Item::ECKey(key))) => PrivateKey(key),
        Some(Err(e)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("could not parse the key: {e}"),
            ))
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a private key",
            ))
        }
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not find a private key",
            ))
        }
    };

    if key_iterator.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected exactly one private key",
        ));
    }
    Ok(private_key)
}

/// Configuration options pertaining to the subgraph server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct TlsSubgraph {
    /// list of certificate authorities in PEM format
    pub(crate) certificate_authorities: Option<String>,
    /// client certificate authentication
    pub(crate) client_authentication: Option<TlsClientAuth>,
}

#[buildstructor::buildstructor]
impl TlsSubgraph {
    #[builder]
    pub(crate) fn new(
        certificate_authorities: Option<String>,
        client_authentication: Option<TlsClientAuth>,
    ) -> Self {
        Self {
            certificate_authorities,
            client_authentication,
        }
    }
}

impl Default for TlsSubgraph {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// TLS client authentication
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsClientAuth {
    /// list of certificates in PEM format
    #[serde(deserialize_with = "deserialize_certificate_chain", skip_serializing)]
    #[schemars(with = "String")]
    pub(crate) certificate_chain: Vec<Certificate>,
    /// key in PEM format
    #[serde(deserialize_with = "deserialize_key", skip_serializing)]
    #[schemars(with = "String")]
    pub(crate) key: PrivateKey,
}

/// Configuration options pertaining to the sandbox page.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Sandbox {
    /// Set to true to enable sandbox
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

/// Configuration options pertaining to the home page.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Homepage {
    /// Set to false to disable the homepage
    pub(crate) enabled: bool,
    /// Graph reference
    /// This will allow you to redirect from the Apollo Router landing page back to Apollo Studio Explorer
    pub(crate) graph_ref: Option<String>,
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
            graph_ref: None,
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
            graph_ref: None,
        }
    }
}

impl Default for Homepage {
    fn default() -> Self {
        Self::builder().enabled(default_homepage()).build()
    }
}

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct HealthCheck {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:8088
    pub(crate) listen: ListenAddr,

    /// Set to false to disable the health check endpoint
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
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl HealthCheck {
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

/// Configuration for chaos testing, trying to reproduce bugs that require uncommon conditions.
/// You probably don’t want this in production!
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Chaos {
    /// Force a hot reload of the Router (as if the schema or configuration had changed)
    /// at a regular time interval.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "Option<String>")]
    pub(crate) force_reload: Option<std::time::Duration>,
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

#[allow(clippy::from_over_into)]
impl Into<serde_json::Value> for ListenAddr {
    fn into(self) -> serde_json::Value {
        match self {
            // It avoids to prefix with `http://` when serializing and relying on the Display impl.
            // Otherwise, it's converted to a `UnixSocket` in any case.
            Self::SocketAddr(addr) => serde_json::Value::String(addr.to_string()),
            #[cfg(unix)]
            Self::UnixSocket(path) => serde_json::Value::String(
                path.as_os_str()
                    .to_str()
                    .expect("unsupported non-UTF-8 path")
                    .to_string(),
            ),
        }
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
            Self::SocketAddr(addr) => write!(f, "http://{addr}"),
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
