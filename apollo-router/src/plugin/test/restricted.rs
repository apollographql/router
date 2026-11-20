use async_trait::async_trait;
use tower::BoxError;

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;

/// Restricted plugin (for testing purposes only)
#[apollo_configuration::configuration]
struct Config {
    /// Enable the restricted plugin (for testing purposes only)
    #[config(required)]
    enabled: bool,
}

/// Dummy plugin (for testing purposes only)
struct Restricted;

register_private_plugin!("experimental", "restricted", Restricted);

#[async_trait]
impl PluginPrivate for Restricted {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        tracing::info!("restricted plugin enabled: {}", init.config.enabled);
        Ok(Restricted)
    }
}
