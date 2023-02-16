use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;

/// Restricted plugin (for testing purposes only)
#[derive(Deserialize, JsonSchema)]
struct Config {
    /// Enable the restricted plugin (for testing purposes only)
    enabled: bool,
}

/// Dummy plugin (for testing purposes only)
struct Restricted;

register_plugin!("experimental", "restricted", Restricted);

#[async_trait]
impl Plugin for Restricted {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        tracing::info!("restricted plugin enabled: {}", init.config.enabled);
        Ok(Restricted)
    }
}
