use apollo_router::plugin::{Plugin, PluginInit};
use apollo_router::register_plugin;
use async_trait::async_trait;
use rand::Rng;
use tower::BoxError;

register_plugin!("test", "flaky", FlakyPlugin);

struct FlakyPlugin;
#[async_trait]
impl Plugin for FlakyPlugin {
    type Config = bool;

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, tower::BoxError>
    where
        Self: Sized,
    {
        let mut rng = rand::thread_rng();
        if rng.gen_bool(0.5) {
            Err(BoxError::from("failed to init"))
        } else {
            Ok(FlakyPlugin)
        }
    }
}
