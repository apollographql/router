//! Plugin settings deserialized while the configuration is parsed.

use std::collections::HashMap;

use serde_json::Map;
use serde_json::Value;

use super::APOLLO_PLUGIN_PREFIX;
use super::ConfigurationError;
use crate::plugin::PluginConfig;
use crate::plugin::plugins;

/// Every configured plugin's settings, deserialized once for construction.
///
/// Invalid settings are kept as errors rather than failing deserialization, so the shared parser's
/// validation can report every one of them against its section of the document.
#[derive(Debug, Default)]
pub(crate) struct PluginConfigs {
    /// Built-in plugins, keyed by full name such as `apollo.telemetry`.
    apollo: HashMap<String, PluginConfig>,
    /// User plugins, in the order the configuration lists them.
    user: Vec<(String, PluginConfig)>,
    /// User plugin sections that name no registered plugin.
    unknown: Vec<String>,
    errors: Vec<PluginConfigError>,
}

/// A plugin whose settings could not be deserialized.
#[derive(Debug)]
pub(crate) struct PluginConfigError {
    /// The plugin's full name.
    pub(crate) plugin: String,
    /// Where the plugin's section is in the document, such as `["plugins", "acme.auth"]`.
    pub(crate) section: Vec<String>,
    pub(crate) error: String,
}

impl PluginConfigError {
    pub(crate) fn to_configuration_error(&self) -> ConfigurationError {
        ConfigurationError::PluginConfiguration {
            plugin: self.plugin.clone(),
            error: self.error.clone(),
        }
    }
}

impl PluginConfigs {
    /// Deserializes the built-in sections (keyed by short name) and the user plugin sections.
    /// A built-in section naming no registered plugin is left to schema validation.
    pub(crate) fn parse(
        apollo_sections: &Map<String, Value>,
        user_sections: &Map<String, Value>,
    ) -> Self {
        let mut configs = Self::default();
        for (name, settings) in apollo_sections {
            let full_name = format!("{APOLLO_PLUGIN_PREFIX}{name}");
            if let Some(config) = configs.parse_section(&full_name, vec![name.clone()], settings) {
                configs.apollo.insert(full_name, config);
            }
        }
        for (name, settings) in user_sections {
            let section = vec!["plugins".to_string(), name.clone()];
            if !plugins().any(|factory| factory.name == *name) {
                configs.unknown.push(name.clone());
            } else if let Some(config) = configs.parse_section(name, section, settings) {
                configs.user.push((name.clone(), config));
            }
        }
        configs
    }

    fn parse_section(
        &mut self,
        plugin: &str,
        section: Vec<String>,
        settings: &Value,
    ) -> Option<PluginConfig> {
        let factory = plugins().find(|factory| factory.name == plugin)?;
        match factory.parse_config(settings.clone()) {
            Ok(config) => Some(config),
            Err(error) => {
                self.errors.push(PluginConfigError {
                    plugin: plugin.to_string(),
                    section,
                    error: error.to_string(),
                });
                None
            }
        }
    }

    /// The settings for the plugin named `full_name`, when it has a valid section.
    pub(crate) fn get(&self, full_name: &str) -> Option<&PluginConfig> {
        self.apollo.get(full_name).or_else(|| {
            self.user
                .iter()
                .find(|(name, _)| name == full_name)
                .map(|(_, config)| config)
        })
    }

    /// User plugins with valid settings, in configuration order.
    pub(crate) fn user_plugins(&self) -> impl Iterator<Item = (&str, &PluginConfig)> {
        self.user
            .iter()
            .map(|(name, config)| (name.as_str(), config))
    }

    /// User plugin sections that name no registered plugin.
    pub(crate) fn unknown_plugins(&self) -> &[String] {
        &self.unknown
    }

    pub(crate) fn errors(&self) -> &[PluginConfigError] {
        &self.errors
    }

    /// Fails with the first invalid plugin's error, for configurations built in code.
    #[cfg(test)]
    pub(crate) fn check(self) -> Result<Self, ConfigurationError> {
        match self.errors.first() {
            Some(error) => Err(error.to_configuration_error()),
            None => Ok(self),
        }
    }
}
