//! Configuration schema generation and validation

use std::sync::OnceLock;

use schemars::Schema;
use schemars::generate::SchemaSettings;

use super::Configuration;
pub(crate) use crate::configuration::upgrade::generate_upgrade;

/// Generate a JSON schema for the configuration.
pub(crate) fn generate_config_schema() -> Schema {
    let settings = SchemaSettings::draft07().with(|s| {
        s.inline_subschemas = false;
    });

    // Manually patch up the schema
    // We don't want to allow unknown fields, but serde doesn't work if we put the annotation on Configuration as the struct has a flattened type.
    // It's fine to just add it here.
    let generator = settings.into_generator();
    let mut schema = generator.into_root_schema_for::<Configuration>();
    schema.insert("additionalProperties".to_string(), false.into());
    schema
}

/// [`generate_config_schema`] as JSON, as the shared-parser adapter applies it, generated once and
/// shared by the adapter and tests.
///
/// Earlier releases accepted `plugins: null`, meaning no user plugins. The published schema only
/// allows an object, so this copy also allows null there. Such a document is then parsed as
/// written, and its diagnostics refer to the file.
pub(crate) fn router_config_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema = serde_json::to_value(generate_config_schema())
            .expect("router's configuration schema serializes");
        let plugins_type = schema
            .pointer_mut("/definitions/Plugins/type")
            .expect("the configuration schema declares the Plugins type");
        *plugins_type = serde_json::json!(["object", "null"]);
        schema
    })
}

/// Checks for hand-written schema defaults.
///
/// Configuration sections that hold secrets are deliberately not serializable, so the schema
/// derive cannot produce their `default` annotations; they declare them by hand instead. These
/// helpers let each section's tests prove that what the schema advertises still deserializes to
/// the section's runtime `Default`.
#[cfg(test)]
pub(crate) mod advertised_defaults {
    use std::fmt::Debug;

    use serde::de::DeserializeOwned;
    use serde_json::Value;

    use super::router_config_schema;

    /// The `default` the generated schema advertises for `property` of `definition`.
    pub(crate) fn of_property(definition: &str, property: &str) -> Value {
        router_config_schema()
            .pointer(&format!(
                "/definitions/{definition}/properties/{property}/default"
            ))
            .cloned()
            .unwrap_or_else(|| panic!("{definition}.{property} advertises no default"))
    }

    /// An object built from the `default` the generated schema advertises for every property of
    /// `definition`. Fails if any property advertises none.
    pub(crate) fn of_every_property(definition: &str) -> Value {
        let properties = router_config_schema()
            .pointer(&format!("/definitions/{definition}/properties"))
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{definition} declares no properties"));
        properties
            .iter()
            .map(|(name, property)| {
                let default = property
                    .get("default")
                    .unwrap_or_else(|| panic!("{definition}.{name} advertises no default"));
                (name.clone(), default.clone())
            })
            .collect::<serde_json::Map<_, _>>()
            .into()
    }

    /// Asserts that `advertised` deserializes to `T::default()` and returns the parsed value.
    ///
    /// Compares `Debug` output because secret-bearing configurations do not implement
    /// `PartialEq`. `Debug` hides redacted values, so callers must compare those explicitly.
    pub(crate) fn assert_describes_default<T>(advertised: Value) -> T
    where
        T: DeserializeOwned + Default + Debug,
    {
        let parsed: T = serde_json::from_value(advertised.clone())
            .unwrap_or_else(|error| panic!("advertised default {advertised} is invalid: {error}"));
        assert_eq!(
            format!("{parsed:?}"),
            format!("{:?}", T::default()),
            "advertised default {advertised} differs from the runtime default"
        );
        parsed
    }
}
