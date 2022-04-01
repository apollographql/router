use derivative::Derivative;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use url::Url;

#[derive(Debug, Clone, Derivative, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[derivative(Default)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Endpoint {
    Agent(SocketAddr),
    Collector(Url),
}
