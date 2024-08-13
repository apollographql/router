//! Subgraph configuration override behaviour

use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::marker::PhantomData;

use schemars::JsonSchema;
use serde::de;
use serde::de::DeserializeOwned;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Serialize;

// In various parts of the configuration, we need to provide a global configuration for subgraphs,
// with a per subgraph override. This cannot be handled easily with `Default` implementations,
// because to work in an intuitive way, overriding configuration should work per field, not on the
// entire structure.
//
// As an example, let's say we have this subgraph plugin configuration:
//
// ```rust
// use serde::Deserialize;
// use serde::Serialize;
// use schemars::JsonSchema;
//
// #[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
// struct PluginConfig {
//     #[serde(default = "set_true")]
//     a: bool,
//     #[serde(default)]
//     b: u8,
// }
// ```
//
// If we have this configuration, we expect that all subgraph would have `a = false`, except for the
// "products" subgraph. All subgraphs would have `b = 0`, the default.
// ```yaml
// subgraph:
//   all:
//     a: false
//   subgraphs:
//     products:
//       a: true
// ```
//
// But now, if we get this configuration:
//
// ```yaml
// subgraph:
//   all:
//     a: false
//   subgraphs:
//     products:
//       b: 1
// ```
//
// We would expect that:
// - for all subgraphs, `a = false`
// - for all subgraphs, `b = 0`
// - for the "products" subgraph, `b = 1`
//
// Unfortunately, if we used `Default` implementation, we would end up with `a = true` for the
// "products" subgraph.
//
// Another way to handle it is to use `Option` for every field, then handle override when requesting them,
// but this ends up with a configuration schema that does not contain the default values.
//
// This `SubgraphConfiguration` type handles overrides through a custom deserializer that works in three steps:
// - deserialize `all` and `subgraphs` fields to `serde_yaml::Mapping`
// - for each specific subgraph configuration, start from the `all` configuration (or default implementation),
// and replace the overriden fields
// - deserialize to the plugin configuration

/// Configuration options pertaining to the subgraph server component.
#[derive(Default, Serialize, JsonSchema)]
pub(crate) struct SubgraphConfiguration<T>
where
    T: Default + Serialize + JsonSchema,
{
    /// options applying to all subgraphs
    #[serde(default)]
    pub(crate) all: T,
    /// per subgraph options
    #[serde(default)]
    pub(crate) subgraphs: HashMap<String, T>,
}

impl<T> SubgraphConfiguration<T>
where
    T: Default + Serialize + JsonSchema,
{
    #[allow(dead_code)]
    pub(crate) fn get(&self, subgraph_name: &str) -> &T {
        self.subgraphs.get(subgraph_name).unwrap_or(&self.all)
    }
}

impl<T> Debug for SubgraphConfiguration<T>
where
    T: Debug + Default + Serialize + JsonSchema,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubgraphConfiguration")
            .field("all", &self.all)
            .field("subgraphs", &self.subgraphs)
            .finish()
    }
}

impl<T> Clone for SubgraphConfiguration<T>
where
    T: Clone + Default + Serialize + JsonSchema,
{
    fn clone(&self) -> Self {
        Self {
            all: self.all.clone(),
            subgraphs: self.subgraphs.clone(),
        }
    }
}

impl<T> PartialEq for SubgraphConfiguration<T>
where
    T: Default + Serialize + JsonSchema + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.all == other.all && self.subgraphs == other.subgraphs
    }
}

impl<'de, T> Deserialize<'de> for SubgraphConfiguration<T>
where
    T: DeserializeOwned,
    T: Default + Serialize + JsonSchema,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(SubgraphVisitor { t: PhantomData })
    }
}

struct SubgraphVisitor<T> {
    t: PhantomData<T>,
}

impl<'de, T> Visitor<'de> for SubgraphVisitor<T>
where
    T: DeserializeOwned,
    T: Default + Serialize + JsonSchema,
{
    type Value = SubgraphConfiguration<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("struct Subgraph")
    }

    fn visit_map<V>(self, mut map: V) -> Result<SubgraphConfiguration<T>, V::Error>
    where
        V: MapAccess<'de>,
    {
        let mut all: Option<serde_yaml::Mapping> = None;
        let mut parsed_subgraphs: Option<HashMap<String, serde_yaml::Mapping>> = None;
        while let Some(key) = map.next_key()? {
            match key {
                Field::All => {
                    if all.is_some() {
                        return Err(de::Error::duplicate_field("all"));
                    }
                    all = Some(map.next_value()?);
                }
                Field::Subgraphs => {
                    if parsed_subgraphs.is_some() {
                        return Err(de::Error::duplicate_field("subgraphs"));
                    }
                    parsed_subgraphs = Some(map.next_value()?);
                }
            }
        }

        let mut subgraphs = HashMap::new();
        if let Some(subs) = parsed_subgraphs {
            for (subgraph_name, parsed_value) in subs {
                // if `all` was set, use the fields it set, then overwrite with the subgraph
                // specific values
                let value = if let Some(mut value) = all.clone() {
                    for (k, v) in parsed_value {
                        value.insert(k, v);
                    }

                    value
                } else {
                    parsed_value
                };

                let config = serde_yaml::from_value(serde_yaml::Value::Mapping(value))
                    .map_err(de::Error::custom)?;
                subgraphs.insert(subgraph_name, config);
            }
        }

        let all = serde_yaml::from_value(serde_yaml::Value::Mapping(all.unwrap_or_default()))
            .map_err(de::Error::custom)?;

        Ok(SubgraphConfiguration { all, subgraphs })
    }
}

enum Field {
    All,
    Subgraphs,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

struct FieldVisitor;

impl<'de> Visitor<'de> for FieldVisitor {
    type Value = Field;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("`all` or `subgraphs`")
    }

    fn visit_str<E>(self, value: &str) -> Result<Field, E>
    where
        E: de::Error,
    {
        match value {
            "all" => Ok(Field::All),
            "subgraphs" => Ok(Field::Subgraphs),
            _ => Err(de::Error::unknown_field(value, FIELDS)),
        }
    }
}

const FIELDS: &[&str] = &["all", "subgraphs"];
