use apollo_router::plugin::{Plugin, PluginInit};
use apollo_router::register_plugin;
use apollo_router::services::{execution, subgraph, supergraph};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::{BoxError, ServiceBuilder, ServiceExt};
use async_trait::async_trait; // 👈 garante que o import esteja explícito
use valuable::Valuable;

// --- TSH-23556 (Capital One) repro structs ---
// Mirrors the customer's simplified structs from the ticket (comment from
// Zachary Dremann, 2026-07-15). These need the `Valuable` derive so
// `tracing::field::valuable(...)` can carry them into the JSON formatter as
// structured objects instead of falling back to `record_debug`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Valuable)]
struct Entitlements {
    bypass: Option<bool>,
    enforcement_enabled: Option<bool>,
    rhai_script_execution_us: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Valuable)]
struct AccessLog {
    client_id: String,
    entitlements: Entitlements,
    persisted_query_id: Option<String>,
}

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

                // TSH-23556 (Capital One) repro: same call site pattern as the
                // customer's plugin — `info!(log = tracing::field::valuable(&log), ...)`.
                // On router versions before ced4ada00 (#9587) this should render
                // `log` as a nested JSON object. On/after that commit it renders
                // as a flat Debug string instead.
                let log = AccessLog {
                    client_id: "test-client-id".to_string(),
                    entitlements: Entitlements {
                        bypass: None,
                        enforcement_enabled: None,
                        rhai_script_execution_us: None,
                    },
                    persisted_query_id: None,
                };
                tracing::info!(log = tracing::field::valuable(&log), "request finished");

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
// TSH-23556 (Capital One): renamed from "rust.custom_plugin" so the config
// key and any logs/metrics tied to it are identifiable as this ticket's repro.
register_plugin!("capitalone", "tsh_23556_valuable_repro", CustomPlugin);

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn display_message() {
        let config = serde_json::json!({
            "plugins": {
                "capitalone.tsh_23556_valuable_repro": {
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