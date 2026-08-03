//! Sandboxed WebAssembly extensions for the router pipeline.

use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::config::WasmConfig;
use self::layer::WasmSubgraphLayer;
use self::layer::WasmSupergraphLayer;
use self::runtime::Runtime;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::subgraph;
use crate::services::supergraph;

mod config;
mod hooks;
mod layer;
mod runtime;

wasmtime::component::bindgen!({
    path: "wit/router-plugin",
    world: "router-plugin",
    exports: { default: async },
});

pub(super) use self::exports::apollo::router_plugin::hooks as wit;

struct Wasm {
    runtime: Arc<Runtime>,
}

#[async_trait::async_trait]
impl PluginPrivate for Wasm {
    type Config = WasmConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            runtime: Arc::new(Runtime::new(init.config)?),
        })
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneService,
    ) -> supergraph::BoxCloneService {
        ServiceBuilder::new()
            .layer(WasmSupergraphLayer::new(self.runtime.clone()))
            .service(service)
            .boxed_clone()
    }

    fn subgraph_service(
        &self,
        service_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        ServiceBuilder::new()
            .layer(WasmSubgraphLayer::new(
                self.runtime.clone(),
                Arc::from(service_name),
            ))
            .service(service)
            .boxed_clone()
    }
}

register_private_plugin!("apollo", "wasm", Wasm);

#[cfg(test)]
mod tests;
