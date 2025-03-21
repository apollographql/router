use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ConnectorConfiguration<T>
where
    T: Default + Serialize + JsonSchema,
{
    /// Map of subgraph_name.connector_source_name to configuration
    #[serde(default)]
    pub(crate) sources: HashMap<String, T>,

    /// Options applying to all sources
    #[serde(default)]
    pub(crate) all: T,
}
