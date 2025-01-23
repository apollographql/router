use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))] // T does not need to be Default
pub(crate) struct ConnectorConfiguration<T>
where
    T: Serialize + JsonSchema,
{
    // Map of subgraph_name.connector_source_name to configuration
    #[serde(default)]
    pub(crate) sources: HashMap<String, T>,
}
