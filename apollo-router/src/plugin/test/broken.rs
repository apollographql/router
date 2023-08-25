use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;

register_plugin!("experimental", "broken", BrokenPlugin);

/// This is a broken plugin for testing purposes only.
struct BrokenPlugin;

/// This is a broken plugin for testing purposes only.
#[derive(JsonSchema, Deserialize)]
struct Config {
    /// Enable the broken plugin.
    #[serde(rename = "enabled")]
    _enabled: bool,
}

#[async_trait]
impl Plugin for BrokenPlugin {
    type Config = Config;

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Err(BoxError::from("failed to init"))
    }
}
