use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Default, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint: Option<Url>,
    pub apollo_key: Option<String>,
    pub apollo_graph_ref: Option<String>,
}
