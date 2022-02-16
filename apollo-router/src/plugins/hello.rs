use apollo_router_core::{register_plugin, Plugin};
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

struct Hello {}

#[derive(Default, Deserialize, JsonSchema)]
struct Conf {
    name: String,
}

impl Plugin for Hello {
    type Config = Conf;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("Hello {}!", configuration.name);
        Ok(Hello {})
    }
}

register_plugin!("example.com", "hello", Hello);

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use std::str::FromStr;

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("example.com_hello")
            .expect("Plugin not found")
            .create_instance(&Value::from_str("{\"name\":\"Bob\"}").unwrap())
            .unwrap();
    }
}
