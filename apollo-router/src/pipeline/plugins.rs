//! Plugin instantiation for the acquire phase of
//! [`build_pipeline`](super::build_pipeline): ordering, license gating, and construction
//! of every Apollo and user plugin.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::validation::Valid;
use serde_json::Map;
use serde_json::Value;
use tower::BoxError;
use tracing::Instrument;

use crate::AllowedFeature;
use crate::configuration::APOLLO_PLUGIN_PREFIX;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::plugin::DynPlugin;
use crate::plugin::PluginFactory;
use crate::plugin::PluginInit;
use crate::plugins::subscription::notification::Notify;
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::query_planner::SubgraphSchemas;
use crate::services::Plugins;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

/// Processes the plugins in the order below and returns the built instances keyed by name.
///
/// Apart from a pre-activated telemetry instance, which is spliced in directly, each
/// Apollo plugin is added through one of three [`PluginRegistrar`] methods:
///
/// - [`add_mandatory`](PluginRegistrar::add_mandatory) instantiates the plugin even with
///   no user config for it.
/// - [`add_optional`](PluginRegistrar::add_optional) instantiates the plugin only if
///   configured. When the plugin maps to a license-restricted feature, the license's
///   allowed features gate it.
/// - [`add_oss`](PluginRegistrar::add_oss) instantiates the plugin only if configured,
///   with no license check.
pub(crate) async fn create_plugins(
    configuration: &Configuration,
    schema: &Schema,
    subgraph_schemas: Arc<SubgraphSchemas>,
    initial_telemetry_plugin: Option<Box<dyn DynPlugin>>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
    previous_config: Option<Arc<Configuration>>,
) -> Result<Plugins, BoxError> {
    let user_plugins_config = configuration.plugins.clone().plugins.unwrap_or_default();

    // Extract previous plugin configurations for hot reload previous config detection
    let (previous_apollo_plugins_config, previous_user_plugins_config) = match &previous_config {
        Some(config) => {
            // Extract apollo plugin configs from the previous router's stored configuration
            let prev_apollo_configs: HashMap<&str, &Value> = config
                .apollo_plugins
                .plugins
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect();

            // Extract user plugin configs from the previous router's stored configuration
            let prev_user_configs: HashMap<String, &Value> = config
                .plugins
                .plugins
                .as_ref()
                .map(|plugins| plugins.iter().map(|(k, v)| (k.clone(), v)).collect())
                .unwrap_or_default();

            (prev_apollo_configs, prev_user_configs)
        }
        None => (HashMap::new(), HashMap::new()),
    };
    let extra = extra_plugins.unwrap_or_default();
    let apollo_telemetry_plugin_mandatory = apollo_opentelemetry_initialized();

    let mut registrar = PluginRegistrar {
        factories: crate::plugin::PLUGINS
            .iter()
            .filter(|factory| {
                // the name starts with apollo
                factory.name.starts_with(APOLLO_PLUGIN_PREFIX)
                    && (
                        // the plugin is mandatory
                        apollo_telemetry_plugin_mandatory ||
                        // the name isn't apollo.telemetry
                        factory.name != "apollo.telemetry"
                    )
            })
            .map(|factory| (factory.name.as_str(), &**factory))
            .collect(),
        apollo_plugins_config: configuration.apollo_plugins.clone().plugins,
        previous_apollo_plugins_config,
        previous_user_plugins_config,
        plugin_instances: Plugins::default(),
        errors: Vec::new(),
        supergraph_sdl: schema.as_string().clone(),
        supergraph_schema_id: schema.schema_id.clone().into_inner(),
        supergraph_schema: Arc::new(schema.supergraph_schema().clone()),
        subgraph_schemas,
        launch_id: schema.launch_id.clone(),
        notify: configuration.notify.clone(),
        license,
        raw_yaml: configuration.raw_yaml.clone(),
        validated_yaml: configuration.validated_yaml.clone(),
    };

    // Be careful with this list! Moving things around can have subtle consequences.
    // Requests flow through this list multiple times in two directions. First, they go "down"
    // through the list several times as requests at the different services. Then, they go
    // "up" through the list as a response several times, once for each service.
    //
    // The order of this list determines the relative order of plugin hooks executing at each
    // service. This is *not* the same as the order a request flows through the router.
    // For example, assume these three plugins:
    // 1. header propagation (has a hook at the subgraph service)
    // 2. telemetry (has hooks at router, supergraph, and subgraph services)
    // 3. rate limiting (has a hook at the router service)
    // The order here means that header propagation happens before telemetry *at the subgraph
    // service*.
    // Similarly, header propagation being first does not mean that it's exempt from rate
    // limiting, for the same reason. Rate limiting must be after telemetry, though, because
    // telemetry and rate limiting both work at the router service, and requests rejected from
    // the router service must flow through telemetry so we can record errors.
    //
    // Broadly, for telemetry to record errors, we must make sure the telemetry plugin runs
    // before any plugin that can *reject* a request at the router service. Plugins whose
    // router-service hook is an infallible `map_request` (eg `headers`, which only injects
    // `MaskingRulesMap` into context) may appear before telemetry without breaking this
    // invariant — they can't short-circuit a request away from telemetry.
    //
    // Each entry below names the services the plugin hooks. Two plugins whose hooked
    // services don't overlap can be reordered relative to each other; the annotations make
    // that check possible without reading every plugin's source.
    registrar.add_mandatory("include_subgraph_errors").await; // supergraph, subgraph
    registrar.add_mandatory("headers").await; // router, subgraph, connector_request
    if apollo_telemetry_plugin_mandatory {
        match initial_telemetry_plugin {
            None => {
                // router, supergraph, execution, subgraph, connector_request, http_client —
                // must come before any plugin below that can reject a request at the router
                // service, so telemetry records the rejection.
                registrar.add_mandatory("telemetry").await;
            }
            Some(plugin) => {
                let _ = registrar
                    .plugin_instances
                    .insert("apollo.telemetry".to_string(), plugin);
                registrar.apollo_plugins_config.remove("apollo.telemetry");
                registrar.factories.remove("apollo.telemetry");
            }
        }
    }
    registrar.add_mandatory("license_enforcement").await; // router
    registrar.add_mandatory("health_check").await; // router
    registrar.add_mandatory("traffic_shaping").await; // router, subgraph, connector_request
    registrar.add_mandatory("limits").await; // router, subgraph, connector_request
    registrar.add_mandatory("csrf").await; // router
    registrar.add_mandatory("fleet_detector").await; // router, http_client
    registrar.add_mandatory("enhanced_client_awareness").await; // supergraph
    registrar.add_mandatory("experimental_diagnostics").await; // no service hooks

    registrar.add_oss("forbid_mutations").await; // execution
    registrar.add_optional("subscription").await; // subgraph
    registrar.add_oss("override_subgraph_url").await; // subgraph
    registrar.add_optional("authorization").await; // supergraph, execution
    registrar.add_optional("authentication").await; // router, subgraph, connector_request
    registrar.add_oss("preview_file_uploads").await; // router, supergraph, execution, subgraph, http_client
    registrar.add_mandatory("progressive_override").await; // router, supergraph
    registrar.add_optional("demand_control").await; // execution, subgraph

    // This relative ordering is documented publicly for native plugins
    // (/graphos/routing/customization/native-plugins):
    registrar.add_oss("connectors").await; // supergraph, execution
    registrar.add_oss("rhai").await; // router, supergraph, execution, subgraph
    registrar.add_optional("coprocessor").await; // router, supergraph, execution, subgraph, connector_request
    registrar.add_optional("response_cache").await; // supergraph, subgraph
    registrar.add_user_plugins(user_plugins_config, extra).await;

    // Because this plugin intercepts subgraph requests
    // and does not forward them to the next service in the chain,
    // it needs to intervene after user plugins for users plugins to run at all.
    registrar.add_optional("experimental_mock_subgraphs").await; // subgraph

    registrar.finish()
}

/// Construction-time state shared by every plugin instantiation in [`create_plugins`].
///
/// [`add_mandatory`](Self::add_mandatory), [`add_optional`](Self::add_optional), and
/// [`add_oss`](Self::add_oss) each claim their factory out of `factories`, take the
/// plugin's section out of `apollo_plugins_config`, and record the built instance or
/// the construction error. Bundling the state into one struct lets the methods borrow
/// it mutably as a unit.
struct PluginRegistrar<'a> {
    /// Apollo plugin factories not yet claimed by an `add_*` call, keyed by full plugin
    /// name (`apollo.<name>`). [`finish`](Self::finish) panics on any leftovers.
    factories: HashMap<&'static str, &'static PluginFactory>,
    /// Per-plugin config sections not yet consumed, keyed by short plugin name.
    apollo_plugins_config: Map<String, Value>,
    previous_apollo_plugins_config: HashMap<&'a str, &'a Value>,
    previous_user_plugins_config: HashMap<String, &'a Value>,
    plugin_instances: Plugins,
    errors: Vec<ConfigurationError>,
    supergraph_sdl: Arc<String>,
    supergraph_schema_id: Arc<String>,
    supergraph_schema: Arc<Valid<apollo_compiler::Schema>>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    launch_id: Option<Arc<String>>,
    notify: Notify<String, crate::graphql::Response>,
    license: Arc<LicenseState>,
    raw_yaml: Option<Arc<str>>,
    /// The full validated configuration, handed only to the telemetry plugin.
    validated_yaml: Option<Value>,
}

impl PluginRegistrar<'_> {
    /// The span covering one plugin's construction. `info_span!` requires a const span
    /// name, so the plugin-specific name goes in `otel.name`, which the OpenTelemetry
    /// layer exports as the span name.
    fn plugin_span(full_name: &str) -> tracing::Span {
        tracing::info_span!(
            "plugin",
            otel.name = format!("plugin: {full_name}").as_str()
        )
    }

    /// Claims the factory for a plugin out of `factories`, panicking if the plugin was
    /// never registered or was claimed twice.
    fn take_factory(&mut self, full_name: &str) -> &'static PluginFactory {
        self.factories
            .remove(full_name)
            .unwrap_or_else(|| panic!("Apollo plugin not registered: {full_name}"))
    }

    /// Instantiates the Apollo plugin named `apollo.<name>` even when the configuration
    /// has no section for it, defaulting the config to an empty object.
    async fn add_mandatory(&mut self, name: &str) {
        let full_name = format!("apollo.{name}");
        let span = Self::plugin_span(&full_name);
        async {
            let factory = self.take_factory(&full_name);
            let mut plugin_config = self
                .apollo_plugins_config
                .remove(name)
                .unwrap_or(Value::Object(Map::new()));
            let mut full_config = None;
            if full_name == "apollo.telemetry" {
                // The apollo.telemetry plugin isn't happy with empty config, so we
                // give it some. If any of the other mandatory plugins need special
                // treatment, then we'll have to perform it here
                inject_schema_id(&self.supergraph_schema_id, &mut plugin_config);

                // Only the telemetry plugin should have access to the full configuration
                full_config = self.validated_yaml.clone();
            }
            let previous_config = self.previous_apollo_plugins_config.get(name).copied();
            self.add_plugin(
                full_name,
                factory,
                &plugin_config,
                previous_config,
                full_config,
            )
            .await;
        }
        .instrument(span)
        .await;
    }

    /// Instantiates the Apollo plugin named `apollo.<name>` when the configuration has a
    /// section for it. When the plugin maps to a license-restricted feature and the
    /// license does not allow that feature, the method skips the plugin and logs a
    /// warning.
    async fn add_optional(&mut self, name: &str) {
        let full_name = format!("apollo.{name}");
        let span = Self::plugin_span(&full_name);
        async {
            let factory = self.take_factory(&full_name);
            let Some(plugin_config) = self.apollo_plugins_config.remove(name) else {
                return;
            };
            // A plugin whose name maps to no restricted feature is not license-gated.
            let allowed = match AllowedFeature::from_plugin_name(name) {
                Some(feature) => self.license.get_allowed_features().contains(&feature),
                None => true,
            };
            if allowed {
                let previous_config = self.previous_apollo_plugins_config.get(name).copied();
                self.add_plugin(full_name, factory, &plugin_config, previous_config, None)
                    .await;
            } else {
                tracing::warn!(
                    "{full_name} plugin is not registered, {full_name} is a restricted feature that requires a license"
                );
            }
        }
        .instrument(span)
        .await;
    }

    /// Instantiates the Apollo plugin named `apollo.<name>` when the configuration has a
    /// section for it, without a license check.
    async fn add_oss(&mut self, name: &str) {
        let full_name = format!("apollo.{name}");
        let span = Self::plugin_span(&full_name);
        async {
            let factory = self.take_factory(&full_name);
            if let Some(plugin_config) = self.apollo_plugins_config.remove(name) {
                let previous_config = self.previous_apollo_plugins_config.get(name).copied();
                self.add_plugin(full_name, factory, &plugin_config, previous_config, None)
                    .await;
            }
        }
        .instrument(span)
        .await;
    }

    /// Instantiates every configured user plugin in configuration order, then appends
    /// the pre-built `extra` instances (supplied by tests) verbatim.
    async fn add_user_plugins(
        &mut self,
        user_plugins_config: Map<String, Value>,
        extra: Vec<(String, Box<dyn DynPlugin>)>,
    ) {
        for (name, plugin_config) in user_plugins_config {
            let user_span = tracing::info_span!("user_plugin", "name" = &name);
            async {
                if let Some(factory) = crate::plugin::PLUGINS
                    .iter()
                    .find(|factory| factory.name == name)
                {
                    let previous_config = self.previous_user_plugins_config.get(&name).copied();
                    self.add_plugin(name, factory, &plugin_config, previous_config, None)
                        .await;
                } else {
                    self.errors.push(ConfigurationError::PluginUnknown(name))
                }
            }
            .instrument(user_span)
            .await;
        }

        self.plugin_instances.extend(extra);
    }

    /// Builds the [`PluginInit`] for one plugin and instantiates it through `factory`.
    /// Pushes a construction failure onto `errors` instead of returning it. One broken
    /// plugin therefore does not hide the others' errors.
    async fn add_plugin(
        &mut self,
        name: String,
        factory: &PluginFactory,
        plugin_config: &Value,
        previous_plugin_config: Option<&Value>,
        full_config: Option<Value>,
    ) {
        let plugin_init = PluginInit::builder()
            .config(plugin_config.clone())
            .and_previous_config(previous_plugin_config.cloned())
            .supergraph_sdl(self.supergraph_sdl.clone())
            .supergraph_schema_id(self.supergraph_schema_id.clone())
            .supergraph_schema(self.supergraph_schema.clone())
            .subgraph_schemas(self.subgraph_schemas.clone())
            .launch_id(self.launch_id.clone())
            .notify(self.notify.clone())
            .license(self.license.clone())
            .and_full_config(full_config)
            .and_original_config_yaml(self.raw_yaml.clone())
            .build();

        match factory.create_instance(plugin_init).await {
            Ok(plugin) => {
                let _ = self.plugin_instances.insert(name, plugin);
            }
            Err(err) => self.errors.push(ConfigurationError::PluginConfiguration {
                plugin: name,
                error: err.to_string(),
            }),
        }
    }

    /// Returns the built plugin instances, or the aggregated configuration errors if any
    /// plugin failed to build.
    ///
    /// # Panics
    /// Panics when a registered Apollo plugin factory was never claimed by an `add_*`
    /// call, meaning the plugin is missing from the ordering list in [`create_plugins`].
    fn finish(self) -> Result<Plugins, BoxError> {
        let unused_apollo_plugin_names = self.factories.keys().copied().collect::<Vec<_>>();
        if !unused_apollo_plugin_names.is_empty() {
            panic!(
                "Apollo plugins without their ordering specified in `fn create_plugins`: {}",
                unused_apollo_plugin_names.join(", ")
            )
        }

        let plugin_details = self
            .plugin_instances
            .iter()
            .map(|(name, plugin)| (name, plugin.name()))
            .collect::<Vec<(&String, &str)>>();
        tracing::debug!(
            "plugins list: {:?}",
            plugin_details
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<&&String>>()
        );

        if !self.errors.is_empty() {
            for error in &self.errors {
                tracing::error!("{:#}", error);
            }

            let errors_list = self
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>()
                .join("\n");

            Err(BoxError::from(format!(
                "there were {} configuration errors\n{}",
                self.errors.len(),
                errors_list
            )))
        } else {
            Ok(self.plugin_instances)
        }
    }
}

pub(crate) fn inject_schema_id(
    // Ideally we'd use &SchemaHash, but we'll need to update a bunch of tests to do so
    schema_id: &str,
    configuration: &mut Value,
) {
    if configuration.get("apollo").is_none() {
        // Warning: this must be done here, otherwise studio reporting will not work
        if apollo_key().is_some() && apollo_graph_reference().is_some() {
            if let Some(telemetry) = configuration.as_object_mut() {
                telemetry.insert("apollo".to_string(), Value::Object(Default::default()));
            }
        } else {
            return;
        }
    }
    if let Some(apollo) = configuration.get_mut("apollo")
        && let Some(apollo) = apollo.as_object_mut()
    {
        apollo.insert(
            "schema_id".to_string(),
            Value::String(schema_id.to_string()),
        );
    }
}
