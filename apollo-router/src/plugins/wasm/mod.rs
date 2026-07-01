//! `experimental_wasm_data_sources` plugin.
//!
//! Loads WebAssembly **components** declared in router config and builds the
//! [`WasmComponentServiceFactory`] dispatch registry (subgraph name → compiled component). The
//! factory is pulled out of this plugin by the supergraph creator and threaded into the
//! `FetchService`, so a query-plan fetch to a wasm-backed subgraph is resolved by invoking the
//! component instead of making an HTTP call.
//!
//! Config (top-level, since this is an `apollo.*` plugin):
//!
//! ```yaml
//! experimental_wasm_data_sources:
//!   incidentio:                       # arbitrary component name
//!     subgraph: incidentio            # supergraph subgraph this component backs
//!     source:
//!       path: ./incidentio.wasm       # OR  oci: ghcr.io/acme/incidentio:1.2.3
//!     config:
//!       INCIDENT_IO_API_KEY: "${env.INCIDENT_IO_API_KEY}"   # exposed via wasi:config/store
//! ```
//!
//! Gated behind the `wasm-components` cargo feature.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::services::wasm_service::WasmComponent;
use crate::services::wasm_service::WasmComponentServiceFactory;
use crate::services::wasm_service::WasmRuntime;

mod loader;

/// Plugin config: a map of component name → its configuration.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub(crate) struct Conf {
    pub(crate) sources: BTreeMap<String, ComponentConf>,
}

/// Configuration for a single wasm component data source.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct ComponentConf {
    /// The supergraph subgraph name this component backs. A query-plan fetch to this subgraph is
    /// dispatched to the component.
    pub(crate) subgraph: String,
    /// Where to load the `.wasm` component from.
    pub(crate) source: Source,
    /// Configuration values exposed to the component via `wasi:config/store` (e.g. API keys).
    /// Values support the router's `${env.VAR}` expansion.
    #[serde(default)]
    pub(crate) config: BTreeMap<String, String>,
}

/// Where a component binary comes from. Exactly one of `path` / `oci` must be set.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct Source {
    /// Local filesystem path to the `.wasm` component, relative to the working directory.
    #[serde(default)]
    pub(crate) path: Option<PathBuf>,
    /// OCI image reference to pull the component from (operator-propagated artifact).
    #[serde(default)]
    pub(crate) oci: Option<String>,
}

/// The plugin. Holds the dispatch registry built at startup from config.
pub(crate) struct WasmDataSources {
    service_factory: Arc<WasmComponentServiceFactory>,
}

impl WasmDataSources {
    /// The dispatch registry, for the supergraph creator to thread into the `FetchService`.
    pub(crate) fn service_factory(&self) -> Arc<WasmComponentServiceFactory> {
        self.service_factory.clone()
    }
}

#[async_trait::async_trait]
impl Plugin for WasmDataSources {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        // One shared engine + linker for all components.
        let runtime = Arc::new(WasmRuntime::new().map_err(|e| e.to_string())?);

        let mut components: IndexMap<Arc<str>, Arc<WasmComponent>> = IndexMap::new();
        for (name, conf) in &init.config.sources {
            let bytes = loader::load(&conf.source)
                .await
                .map_err(|e| format!("wasm data source `{name}`: {e}"))?;
            let component = WasmComponent::compile(runtime.clone(), &bytes, conf.config.clone())
                .map_err(|e| format!("wasm data source `{name}`: {e}"))?;
            if components
                .insert(Arc::from(conf.subgraph.as_str()), Arc::new(component))
                .is_some()
            {
                return Err(format!(
                    "two wasm data sources target the same subgraph `{}`",
                    conf.subgraph
                )
                .into());
            }
        }

        Ok(Self {
            service_factory: Arc::new(WasmComponentServiceFactory::new(components)),
        })
    }
}

crate::register_plugin!("apollo", "experimental_wasm_data_sources", WasmDataSources);
