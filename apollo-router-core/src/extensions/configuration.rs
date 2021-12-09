use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Extension {
    Wasm(Wasm),
    Dll(Dll),
    Static(Static),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Wasm {
    pub path: String,
    #[serde(flatten)]
    pub config: PluginConfiguration,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Dll {
    pub path: String,
    #[serde(flatten)]
    pub config: PluginConfiguration,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Static {
    pub name: String,
    #[serde(flatten)]
    pub config: PluginConfiguration,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct PluginConfiguration {
    #[serde(default)]
    pub hooks: Vec<HookPoint>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub meta: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HookPoint {
    pub kind: HookPointKind,
    // crude way to order plugin execution if multiple plugins use the same hooks
    #[serde(default)]
    pub weight: u8,
}
#[derive(Debug, Clone, Hash, PartialEq, Eq, Deserialize, Serialize)]
pub enum HookPointKind {
    RequestDidStart,
    DidResolveSource,
    DidResolveOperation,
    // etc
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Capability {
    Network,
    File { path: String },
}
