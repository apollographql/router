//! Sandboxed WebAssembly extensions for the router pipeline.

use std::ops::ControlFlow;
use std::sync::Arc;

use tower::BoxError;
use tower::Service;
use tower::ServiceExt;
use tower::service_fn;

use self::config::WasmConfig;
use self::runtime::Runtime;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::subgraph;
use crate::services::supergraph;

mod config;
mod hooks;
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
        let runtime = self.runtime.clone();
        tower::util::BoxCloneService::new(service_fn(move |request: supergraph::Request| {
            let runtime = runtime.clone();
            let mut service = service.clone();
            async move {
                match runtime.process_supergraph_request(request).await? {
                    ControlFlow::Continue(request) => service.ready().await?.call(request).await,
                    ControlFlow::Break(response) => Ok(response),
                }
            }
        }))
    }

    fn subgraph_service(
        &self,
        service_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        let runtime = self.runtime.clone();
        let service_name: Arc<str> = Arc::from(service_name);
        tower::util::BoxCloneService::new(service_fn(move |request: subgraph::Request| {
            let runtime = runtime.clone();
            let service_name = service_name.clone();
            let mut service = service.clone();
            async move {
                match runtime
                    .process_subgraph_request(request, &service_name)
                    .await?
                {
                    ControlFlow::Continue(request) => service.ready().await?.call(request).await,
                    ControlFlow::Break(response) => Ok(response),
                }
            }
        }))
    }
}

register_private_plugin!("apollo", "wasm", Wasm);

#[cfg(test)]
mod tests;
