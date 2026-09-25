//! Plugin config deserialized when the configuration is parsed.

use std::collections::HashMap;

use serde_json::Map;
use serde_json::Value;

use super::APOLLO_PLUGIN_PREFIX;
use super::ConfigurationError;
use crate::plugin::PluginConfig;
use crate::plugin::PluginFactory;
use crate::plugin::plugins;

/// Every configured plugin's config, deserialized once for construction.
///
/// Invalid config is kept as errors rather than failing deserialization, so configuration parsing
/// can report every one of them against its section of the document.
#[derive(Debug, Default)]
pub(crate) struct PluginConfigs {
    /// Built-in plugins, keyed by full name such as `apollo.telemetry`.
    apollo: HashMap<String, ParsedPlugin>,
    /// User plugins, in the order the configuration lists them.
    user: Vec<(String, ParsedPlugin)>,
    /// User plugin sections that name no registered plugin.
    unknown: Vec<String>,
    errors: Vec<PluginConfigError>,
}

/// A plugin's config and the factory that builds the plugin from it.
#[derive(Clone, Debug)]
pub(crate) struct ParsedPlugin {
    pub(crate) factory: &'static PluginFactory,
    pub(crate) config: PluginConfig,
}

/// A plugin whose config could not be deserialized.
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
    pub(crate) fn parse(
        apollo_sections: &Map<String, Value>,
        user_sections: &Map<String, Value>,
    ) -> Self {
        let mut configs = Self::default();
        for (name, config) in apollo_sections {
            let full_name = format!("{APOLLO_PLUGIN_PREFIX}{name}");
            let section = vec![name.clone()];
            match find_factory(&full_name) {
                Some(factory) => {
                    if let Some(parsed) = configs.parse_section(factory, section, config) {
                        configs.apollo.insert(full_name, parsed);
                    }
                }
                // The schema rejects unknown top-level keys, so a section that no plugin reads
                // means a field is missing from `impl Deserialize for Configuration`.
                None if name != "server" && name != "plugins" => {
                    configs.errors.push(PluginConfigError {
                        plugin: full_name,
                        section,
                        error: "no plugin reads this section".to_string(),
                    });
                }
                None => {}
            }
        }
        for (name, config) in user_sections {
            let section = vec!["plugins".to_string(), name.clone()];
            match find_factory(name) {
                Some(factory) => {
                    if let Some(parsed) = configs.parse_section(factory, section, config) {
                        configs.user.push((name.clone(), parsed));
                    }
                }
                None => configs.unknown.push(name.clone()),
            }
        }
        configs
    }

    fn parse_section(
        &mut self,
        factory: &'static PluginFactory,
        section: Vec<String>,
        config: &Value,
    ) -> Option<ParsedPlugin> {
        match factory.parse_config(config.clone()) {
            Ok(config) => Some(ParsedPlugin { factory, config }),
            Err(error) => {
                self.errors.push(PluginConfigError {
                    plugin: factory.name.clone(),
                    section,
                    error: error.to_string(),
                });
                None
            }
        }
    }

    /// The built-in plugin named `full_name`, such as `apollo.telemetry`, when it has a valid
    /// section.
    pub(crate) fn apollo(&self, full_name: &str) -> Option<&ParsedPlugin> {
        self.apollo.get(full_name)
    }

    /// The user plugin named `name`, when it has a valid section.
    pub(crate) fn user(&self, name: &str) -> Option<&ParsedPlugin> {
        self.user
            .iter()
            .find(|(user_name, _)| user_name == name)
            .map(|(_, parsed)| parsed)
    }

    /// User plugins with valid config, in configuration order.
    pub(crate) fn user_plugins(&self) -> impl Iterator<Item = (&str, &ParsedPlugin)> {
        self.user
            .iter()
            .map(|(name, parsed)| (name.as_str(), parsed))
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

fn find_factory(name: &str) -> Option<&'static PluginFactory> {
    plugins()
        .find(|factory| factory.name == name)
        .map(|factory| &**factory)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sections(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    /// A built-in section that no plugin reads is reported, except the `server` and `plugins`
    /// keys, which are not plugin sections.
    #[test]
    fn apollo_sections_without_a_plugin_are_errors() {
        let configs = PluginConfigs::parse(
            &sections(json!({ "no_such_plugin": {}, "server": {}, "plugins": {} })),
            &Map::new(),
        );

        let errors: Vec<(&str, &[String])> = configs
            .errors()
            .iter()
            .map(|error| (error.plugin.as_str(), error.section.as_slice()))
            .collect();
        assert_eq!(
            errors,
            [("apollo.no_such_plugin", &["no_such_plugin".to_string()][..])]
        );
    }

    /// Built-in and user plugins are looked up separately, so a user section named like a
    /// built-in plugin does not configure it.
    #[test]
    fn user_sections_do_not_answer_for_built_in_plugins() {
        let configs = PluginConfigs::parse(
            &Map::new(),
            &sections(json!({ "apollo.forbid_mutations": true })),
        );

        assert!(configs.apollo("apollo.forbid_mutations").is_none());
        let parsed = configs
            .user("apollo.forbid_mutations")
            .expect("the section names a registered plugin");
        assert_eq!(parsed.factory.name, "apollo.forbid_mutations");
    }
}
