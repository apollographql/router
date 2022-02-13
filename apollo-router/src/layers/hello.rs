use apollo_router_core::register_layer;
use apollo_router_core::ConfigurableLayer;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::layer::layer_fn;
use tower::{BoxError, Layer};

#[derive(Default)]
struct Hello {}

#[derive(Default, Deserialize, JsonSchema)]
struct Conf {
    name: String,
}

impl<S> Layer<S> for Hello {
    type Service = S;

    fn layer(&self, inner: S) -> Self::Service {
        layer_fn(|s| s).layer(inner)
    }
}

impl ConfigurableLayer for Hello {
    type Config = Conf;

    fn configure(&mut self, configuration: Self::Config) -> Result<(), BoxError> {
        tracing::info!("Hello {}!", configuration.name);
        Ok(())
    }
}

register_layer!("hello", Hello);

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use std::str::FromStr;

    #[tokio::test]
    async fn layer_registered() {
        apollo_router_core::layers()
            .get("hello")
            .expect("Layer not found")
            .create_instance(&Value::from_str("{\"name\":\"Bob\"}").unwrap())
            .unwrap();
    }
}
