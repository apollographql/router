use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
    pub apollo_key: Option<String>,
    pub apollo_graph_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Endpoint {
    Agent(SocketAddr),
    Collector(Url),
}
