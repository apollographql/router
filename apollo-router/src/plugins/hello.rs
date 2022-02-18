use apollo_router_core::{register_plugin, Plugin};
use schemars::JsonSchema;
use serde::Deserialize;
use std::error::Error;
use std::fmt;
use tower::BoxError;

#[derive(Debug)]
struct HelloError;

impl fmt::Display for HelloError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HelloError")
    }
}

impl Error for HelloError {}

#[derive(Debug)]
struct Hello {
    name: String,
}

impl fmt::Display for Hello {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hello")
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    name: String,
}

#[async_trait::async_trait]
impl Plugin for Hello {
    type Config = Conf;

    async fn startup(&mut self) -> Result<(), BoxError> {
        tracing::info!("starting: {}: {}", stringify!(Hello), self.name);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        tracing::info!("shutting down: {}: {}", stringify!(Hello), self.name);
        Ok(())
    }

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("Hello {}!", configuration.name);
        Ok(Hello {
            name: configuration.name,
        })
    }
}

register_plugin!("com.example", "hello", Hello);

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use std::str::FromStr;

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("com.example.hello")
            .expect("Plugin not found")
            .create_instance(&Value::from_str("{\"name\":\"Bob\"}").unwrap())
            .unwrap();
    }
}
