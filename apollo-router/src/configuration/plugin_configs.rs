//! Plugin config, deserialized with the rest of the configuration.
//!
//! Each plugin's section is deserialized straight from the configuration's own deserializer, so
//! an invalid value is reported at its place in the document, and the typed config is kept for
//! validation and plugin construction.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::de::DeserializeSeed;
use serde::de::MapAccess;
use serde::de::Visitor;

use super::APOLLO_PLUGIN_PREFIX;
use super::ConfigurationError;
use crate::plugin::PluginConfig;
use crate::plugin::PluginFactory;
use crate::plugin::plugins;

/// A configured plugin: the factory that builds it and its typed config.
#[derive(Clone, Debug)]
pub(crate) struct ParsedPlugin {
    pub(crate) factory: &'static PluginFactory,
    pub(crate) config: PluginConfig,
}

/// Built-in plugin sections, keyed by short name such as `telemetry`. Ordered, so validation
/// reports errors in the same order every time.
#[derive(Clone, Debug, Default)]
pub(crate) struct ApolloPlugins {
    sections: BTreeMap<String, ParsedPlugin>,
}

/// User plugin sections under `plugins`, in the order the configuration lists them.
#[derive(Clone, Debug, Default)]
pub(crate) struct UserPlugins {
    sections: Vec<(String, ParsedPlugin)>,
}

impl ApolloPlugins {
    /// The built-in plugin named `full_name`, such as `apollo.telemetry`, when it has a section.
    pub(crate) fn get(&self, full_name: &str) -> Option<&ParsedPlugin> {
        self.sections
            .get(full_name.strip_prefix(APOLLO_PLUGIN_PREFIX)?)
    }

    /// Sections by short name, in name order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &ParsedPlugin)> {
        self.sections
            .iter()
            .map(|(name, parsed)| (name.as_str(), parsed))
    }

    #[cfg(any(test, feature = "mock_subgraphs_testing"))]
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.sections.contains_key(name)
    }

    /// Sets the section `name`, such as `limits`, replacing any section already there.
    pub(crate) fn insert(&mut self, name: &str, parsed: ParsedPlugin) {
        self.sections.insert(name.to_string(), parsed);
    }

    /// Deserializes the value of the current map entry as the section of the built-in plugin
    /// `name`. Returns `false`, leaving the value unread, when no built-in plugin reads it.
    pub(crate) fn next_section<'de, A: MapAccess<'de>>(
        &mut self,
        name: &str,
        map: &mut A,
    ) -> Result<bool, A::Error> {
        let Some(factory) = find_factory(&format!("{APOLLO_PLUGIN_PREFIX}{name}")) else {
            return Ok(false);
        };
        let config = map.next_value_seed(SectionSeed(factory))?;
        self.insert(name, ParsedPlugin { factory, config });
        Ok(true)
    }
}

/// Built-in sections keyed by short name, as the test builders supply them.
impl<'de> Deserialize<'de> for ApolloPlugins {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SectionsVisitor;

        impl<'de> Visitor<'de> for SectionsVisitor {
            type Value = ApolloPlugins;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("built-in plugin sections")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut plugins = ApolloPlugins::default();
                while let Some(name) = map.next_key::<String>()? {
                    if !plugins.next_section(&name, &mut map)? {
                        return Err(serde::de::Error::custom(format!(
                            "no built-in plugin reads the `{name}` section"
                        )));
                    }
                }
                Ok(plugins)
            }
        }

        deserializer.deserialize_map(SectionsVisitor)
    }
}

impl UserPlugins {
    /// The user plugin named `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&ParsedPlugin> {
        self.sections
            .iter()
            .find(|(user_name, _)| user_name == name)
            .map(|(_, parsed)| parsed)
    }

    /// User plugins in configuration order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &ParsedPlugin)> {
        self.sections
            .iter()
            .map(|(name, parsed)| (name.as_str(), parsed))
    }
}

/// The `plugins` section: a map from registered plugin name to its config. `null` means no
/// user plugins, as in earlier releases.
impl<'de> Deserialize<'de> for UserPlugins {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SectionsVisitor;

        impl<'de> Visitor<'de> for SectionsVisitor {
            type Value = UserPlugins;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("plugin sections keyed by plugin name")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(UserPlugins::default())
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(UserPlugins::default())
            }

            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                deserializer.deserialize_map(self)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut plugins = UserPlugins::default();
                while let Some(name) = map.next_key::<String>()? {
                    let factory = find_factory(&name).ok_or_else(|| {
                        serde::de::Error::custom(ConfigurationError::PluginUnknown(name.clone()))
                    })?;
                    let config = map.next_value_seed(SectionSeed(factory))?;
                    plugins
                        .sections
                        .push((name, ParsedPlugin { factory, config }));
                }
                Ok(plugins)
            }
        }

        deserializer.deserialize_option(SectionsVisitor)
    }
}

/// Deserializes one plugin's section with its factory, naming the plugin in the error.
struct SectionSeed(&'static PluginFactory);

impl<'de> DeserializeSeed<'de> for SectionSeed {
    type Value = PluginConfig;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<PluginConfig, D::Error> {
        let mut deserializer = <dyn erased_serde::Deserializer>::erase(deserializer);
        self.0.parse_config(&mut deserializer).map_err(|error| {
            serde::de::Error::custom(ConfigurationError::PluginConfiguration {
                plugin: self.0.name.clone(),
                error: error.to_string(),
            })
        })
    }
}

pub(crate) fn find_factory(name: &str) -> Option<&'static PluginFactory> {
    plugins()
        .find(|factory| factory.name == name)
        .map(|factory| &**factory)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A built-in section that no plugin reads is an error.
    #[test]
    fn apollo_sections_without_a_plugin_are_errors() {
        let error = ApolloPlugins::deserialize(json!({ "no_such_plugin": {} }))
            .expect_err("no plugin reads the section")
            .to_string();

        assert!(error.contains("`no_such_plugin`"), "{error}");
    }

    /// Built-in and user plugins are looked up separately, so a user section named like a
    /// built-in plugin does not configure it.
    #[test]
    fn user_sections_do_not_answer_for_built_in_plugins() {
        let user = UserPlugins::deserialize(json!({ "apollo.forbid_mutations": true }))
            .expect("the section names a registered plugin");

        let parsed = user
            .get("apollo.forbid_mutations")
            .expect("the section names a registered plugin");
        assert_eq!(parsed.factory.name, "apollo.forbid_mutations");
    }

    #[test]
    fn unknown_user_plugins_are_errors() {
        let error = UserPlugins::deserialize(json!({ "acme.missing": {} }))
            .expect_err("no plugin is registered with that name")
            .to_string();

        assert!(error.contains("acme.missing"), "{error}");
    }
}
