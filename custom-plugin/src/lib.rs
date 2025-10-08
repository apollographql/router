use apollo_router::plugin::{Plugin, PluginInit};
use apollo_router::register_plugin;
use apollo_router::services::{execution, subgraph, supergraph};
use schemars::JsonSchema;
use serde::Deserialize;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use async_trait::async_trait; // 👈 garante que o import esteja explícito

#[derive(Debug)]
struct CustomPlugin {
    #[allow(dead_code)]
    configuration: Conf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    /// Example config value. This will be deserialized from the Router YAML/JSON.
    name: String,
}

#[async_trait]
impl Plugin for CustomPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(CustomPlugin {
            configuration: init.config,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        println!("Entering Supergraph Service {}", self.configuration.name);
        tracing::info!("Hello {}", self.configuration.name);

        ServiceBuilder::new()
            .map_request(|req: supergraph::Request| {
                println!(">>> Entering Supergraph Request stage");
                req
            })
            .service(service)
            .map_response(|res: supergraph::Response| {
                println!(">>> Entering Supergraph Response stage");
                res
            })
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .map_request(|req: execution::Request| {
                println!(">>> Entering Execution Request stage");
                req
            })
            .service(service)
            .map_response(|res: execution::Response| {
                println!(">>> Entering Execution Response stage");
                res
            })
            .boxed()
    }

    fn subgraph_service(&self, _name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let n1 = self.configuration.name.clone();
        let n2 = self.configuration.name.clone();

        ServiceBuilder::new()
            .map_request(move |req: subgraph::Request| {
                println!(">>> Entering Subgraph Request stage for {}", n1);
                req
            })
            .service(service)
            .map_response(move |res: subgraph::Response| {
                println!(">>> Entering Subgraph Response stage for {}", n2);
                res
            })
            .boxed()
    }
}

// Register the plugin in the Router plugin registry.
// Format: register_plugin!("group", "name", StructName);
register_plugin!("rust", "custom_plugin", CustomPlugin);

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn display_message() {
        let config = serde_json::json!({
            "plugins": {
                "rust.custom_plugin": {
                    "name": "Bob"
                }
            }
        });

        let _test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }
}

/// Sanity check function, called from `main.rs` to ensure the plugin crate is linked.
pub fn plugin_sanity_check() {
    println!("✅ Custom plugin crate linked and loaded");
}