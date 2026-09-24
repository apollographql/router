//! Plugin instantiation for the acquire phase of
//! [`build_pipeline`](super::build_pipeline): ordering, license gating, and construction
//! of every Apollo and user plugin.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Map;
use serde_json::Value;
use tower::BoxError;
use tracing::Instrument;

use crate::AllowedFeature;
use crate::configuration::APOLLO_PLUGIN_PREFIX;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::plugin::DynPlugin;
use crate::plugin::PluginConfig;
use crate::plugin::PluginFactory;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::query_planner::SubgraphSchemas;
use crate::services::Plugins;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

/// Processes the plugins in the order below and returns the built instances keyed by name.
///
/// Mandatory plugins are instantiated even with no user config for them; optional
/// plugins only when configured. A plugin that maps to a license-restricted feature is
/// skipped, with a warning, when the license does not allow that feature. A
/// pre-activated telemetry instance is spliced in directly instead of being built a
/// second time.
pub(crate) async fn create_plugins(
    configuration: &Configuration,
    schema: &Schema,
    subgraph_schemas: Arc<SubgraphSchemas>,
    initial_telemetry_plugin: Option<Box<dyn DynPlugin>>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
    previous_config: Option<Arc<Configuration>>,
) -> Result<Plugins, BoxError> {
    // The shared parser reports invalid plugin settings. A configuration deserialized directly
    // still carries them, and no plugin is built from it.
    let invalid_settings: Vec<ConfigurationError> = configuration
        .plugin_configs
        .errors()
        .iter()
        .map(|error| error.to_configuration_error())
        .collect();
    if !invalid_settings.is_empty() {
        return Err(configuration_errors(&invalid_settings));
    }

    let user_plugin_names = configuration
        .plugins
        .plugins
        .iter()
        .flat_map(|plugins| plugins.keys().cloned())
        .collect();
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
        configuration,
        previous_config: previous_config.as_deref(),
        plugin_instances: Plugins::default(),
        errors: Vec::new(),
        context: PluginInit::builder()
            .config(())
            .supergraph_sdl(schema.as_string().clone())
            .supergraph_schema_id(schema.schema_id.clone().into_inner())
            .supergraph_schema(Arc::new(schema.supergraph_schema().clone()))
            .subgraph_schemas(subgraph_schemas)
            .launch_id(schema.launch_id.clone())
            .notify(configuration.notify.clone())
            .license(license)
            .and_original_config_yaml(configuration.raw_yaml.clone())
            .build(),
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
    // Two plugins whose hooked services don't overlap can be reordered relative to each
    // other; check each plugin's service hooks before moving an entry.
    registrar.add_mandatory("include_subgraph_errors").await;
    registrar.add_mandatory("headers").await;
    if apollo_telemetry_plugin_mandatory {
        match initial_telemetry_plugin {
            None => {
                // Must come before any plugin below that can reject a request at the
                // router service, so telemetry records the rejection.
                registrar.add_mandatory("telemetry").await;
            }
            Some(plugin) => {
                let _ = registrar
                    .plugin_instances
                    .insert("apollo.telemetry".to_string(), plugin);
                registrar.factories.remove("apollo.telemetry");
            }
        }
    }
    registrar.add_mandatory("health_check").await;
    registrar.add_mandatory("traffic_shaping").await;
    registrar.add_mandatory("limits").await;
    registrar.add_mandatory("csrf").await;
    registrar.add_mandatory("fleet_detector").await;
    registrar.add_mandatory("enhanced_client_awareness").await;
    registrar.add_mandatory("experimental_diagnostics").await;

    registrar.add_optional("forbid_mutations").await;
    registrar.add_optional("subscription").await;
    registrar.add_optional("override_subgraph_url").await;
    registrar.add_optional("authorization").await;
    registrar.add_optional("authentication").await;
    registrar.add_optional("preview_file_uploads").await;
    registrar.add_mandatory("progressive_override").await;
    registrar.add_optional("demand_control").await;

    // This relative ordering is documented publicly for native plugins
    // (/graphos/routing/customization/native-plugins):
    registrar.add_optional("connectors").await;
    registrar.add_optional("rhai").await;
    registrar.add_optional("coprocessor").await;
    registrar.add_optional("response_cache").await;
    registrar.add_optional("expose_query_plan").await;
    registrar.add_user_plugins(user_plugin_names, extra).await;

    // Because this plugin intercepts subgraph requests
    // and does not forward them to the next service in the chain,
    // it needs to intervene after user plugins for users plugins to run at all.
    #[cfg(any(test, feature = "mock_subgraphs_testing"))]
    registrar.add_optional("experimental_mock_subgraphs").await;

    registrar.finish()
}

/// Construction-time state shared by every plugin instantiation in [`create_plugins`].
///
/// [`add_mandatory`](Self::add_mandatory), [`add_optional`](Self::add_optional) and
/// [`add_user_plugins`](Self::add_user_plugins) each claim their factory out of `factories`,
/// construct the plugin from the configuration retained while parsing, and record the built
/// instance or the construction error. Bundling the state into one struct lets the methods
/// borrow it mutably as a unit.
struct PluginRegistrar<'a> {
    /// Apollo plugin factories not yet claimed by an `add_*` call, keyed by full plugin
    /// name (`apollo.<name>`). [`finish`](Self::finish) panics on any leftovers.
    factories: HashMap<&'static str, &'static PluginFactory>,
    configuration: &'a Configuration,
    /// The configuration of the pipeline being replaced, on a hot reload.
    previous_config: Option<&'a Configuration>,
    plugin_instances: Plugins,
    errors: Vec<ConfigurationError>,
    /// The initialisation context every plugin shares; only the configuration differs.
    context: PluginInit<()>,
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
            let plugin_config = match self.configuration.plugin_config(&full_name) {
                Some(config) => config.clone(),
                // Without a section, the plugin runs with its default settings.
                None => match factory.parse_config(Value::Object(Map::new())) {
                    Ok(config) => config,
                    Err(error) => {
                        self.errors.push(ConfigurationError::PluginConfiguration {
                            plugin: full_name,
                            error: error.to_string(),
                        });
                        return;
                    }
                },
            };
            // Only the telemetry plugin should have access to the full configuration
            let full_config = (full_name == "apollo.telemetry")
                .then(|| self.configuration.validated_yaml.clone())
                .flatten();
            self.add_plugin(full_name, factory, plugin_config, full_config)
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
            let Some(plugin_config) = self.configuration.plugin_config(&full_name).cloned() else {
                return;
            };
            // A plugin whose name maps to no restricted feature is not license-gated.
            let allowed = match AllowedFeature::from_plugin_name(name) {
                Some(feature) => self
                    .context
                    .license
                    .get_allowed_features()
                    .contains(&feature),
                None => true,
            };
            if allowed {
                self.add_plugin(full_name, factory, plugin_config, None)
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

    /// Instantiates every configured user plugin in configuration order, then appends
    /// the pre-built `extra` instances (supplied by tests) verbatim.
    async fn add_user_plugins(
        &mut self,
        user_plugin_names: Vec<String>,
        extra: Vec<(String, Box<dyn DynPlugin>)>,
    ) {
        for name in user_plugin_names {
            let user_span = tracing::info_span!("user_plugin", "name" = &name);
            async {
                let factory = crate::plugin::PLUGINS
                    .iter()
                    .find(|factory| factory.name == name);
                let plugin_config = self.configuration.plugin_config(&name).cloned();
                match (factory, plugin_config) {
                    (Some(factory), Some(plugin_config)) => {
                        self.add_plugin(name, factory, plugin_config, None).await
                    }
                    _ => self.errors.push(ConfigurationError::PluginUnknown(name)),
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
        plugin_config: PluginConfig,
        full_config: Option<Value>,
    ) {
        // On a hot reload, the plugin also receives the settings it ran with before.
        let previous_plugin_config = self
            .previous_config
            .and_then(|previous| previous.plugin_config(&name))
            .cloned();
        let mut plugin_init = self
            .context
            .with_config(plugin_config, previous_plugin_config);
        plugin_init.full_config = full_config;

        match factory.create_from_config(plugin_init).await {
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

            Err(configuration_errors(&self.errors))
        } else {
            Ok(self.plugin_instances)
        }
    }
}

/// Combines configuration errors into one error that lists each of them.
fn configuration_errors(errors: &[ConfigurationError]) -> BoxError {
    let errors_list = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join("\n");
    BoxError::from(format!(
        "there were {} configuration errors\n{}",
        errors.len(),
        errors_list
    ))
}
