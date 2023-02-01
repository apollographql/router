use std::{collections::HashMap, fmt, marker::PhantomData};

use schemars::JsonSchema;
use serde::{
    de::{self, DeserializeOwned, MapAccess, Visitor},
    Deserialize, Serialize,
};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct SubgraphConfiguration<T>
where
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
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
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
{
    #[allow(dead_code)]
    fn get(&self, subgraph_name: &str) -> &T {
        self.subgraphs.get(subgraph_name).unwrap_or(&self.all)
    }
}

impl<'de, T> Deserialize<'de> for SubgraphConfiguration<T>
where
    T: DeserializeOwned,
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
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
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
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
        formatter.write_str("`secs` or `nanos`")
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
