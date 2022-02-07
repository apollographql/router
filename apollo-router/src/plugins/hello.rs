use apollo_router_core::Plugin;
use apollo_router_macros::router_plugin;
use serde::Deserialize;
use tower::BoxError;

#[derive(Default)]
struct Hello {}

#[derive(Deserialize)]
struct Conf {
    name: String,
}

#[router_plugin]
impl Plugin for Hello {
    type Config = Conf;

    fn configure(&mut self, configuration: Self::Config) -> Result<(), BoxError> {
        tracing::info!("Hello {}!", configuration.name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use apollo_router_core::DynPlugin;
    use serde_json::Value;
    use std::str::FromStr;

    #[tokio::test]
    async fn plugin_registered() {
        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("hello")
            .expect("Plugin not found")();
        dyn_plugin
            .configure(&Value::from_str("{\"name\":\"Bob\"}").unwrap())
            .expect("Failed to configure");
    }
}
