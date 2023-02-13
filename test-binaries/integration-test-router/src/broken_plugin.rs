use apollo_router::plugin::{Plugin, PluginInit};
use apollo_router::register_plugin;
use async_trait::async_trait;
use rand::Rng;
use tower::BoxError;

register_plugin!("test", "broken", BrokenPlugin);

struct BrokenPlugin;
#[async_trait]
impl Plugin for BrokenPlugin {
    type Config = bool;

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, tower::BoxError>
    where
        Self: Sized,
    {
        Err(BoxError::from("failed to init"))
    }
}
