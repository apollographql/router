use crate::{
    ExecutionRequest, ExecutionResponse, QueryPlannerRequest, QueryPlannerResponse, RouterRequest,
    RouterResponse, SubgraphRequest, SubgraphResponse,
};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tower::util::BoxService;
use tower::BoxError;

type PluginFactory = fn() -> Box<dyn DynPlugin>;

static PLUGIN_REGISTRY: Lazy<Mutex<HashMap<String, PluginFactory>>> = Lazy::new(|| {
    let m = HashMap::new();
    Mutex::new(m)
});

pub fn plugins() -> Arc<HashMap<String, PluginFactory>> {
    Arc::new(PLUGIN_REGISTRY.lock().expect("Lock poisoned").clone())
}

pub fn plugins_mut<'a>() -> MutexGuard<'a, HashMap<String, PluginFactory>> {
    PLUGIN_REGISTRY.lock().expect("Lock poisoned")
}

#[async_trait]
pub trait Plugin: Default + Send + Sync + 'static {
    type Config;

    fn configure(&mut self, _configuration: Self::Config) -> Result<(), BoxError> {
        Ok(())
    }

    // Plugins will receive a notification that they should start up and shut down.
    async fn startup(&mut self) -> Result<(), BoxError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), BoxError> {
        Ok(())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        service
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        service
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        service
    }
}

#[async_trait]
pub trait DynPlugin: Send + Sync + 'static {
    fn configure(&mut self, _configuration: &Value) -> Result<(), BoxError>;

    fn configure_from_json(&mut self, configuration: &serde_json::Value) -> Result<(), BoxError>;

    // Plugins will receive a notification that they should start up and shut down.
    async fn startup(&mut self) -> Result<(), BoxError>;

    async fn shutdown(&mut self) -> Result<(), BoxError>;

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError>;

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>;

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError>;

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError>;
}

#[async_trait]
impl<T> DynPlugin for T
where
    T: Plugin,
    for<'de> <T as Plugin>::Config: Deserialize<'de>,
{
    fn configure(&mut self, configuration: &Value) -> Result<(), BoxError> {
        self.configure_from_json(configuration)
    }

    fn configure_from_json(&mut self, configuration: &serde_json::Value) -> Result<(), BoxError> {
        let conf = serde_json::from_value(configuration.clone())?;
        self.configure(conf)
    }

    // Plugins will receive a notification that they should start up and shut down.
    async fn startup(&mut self) -> Result<(), BoxError> {
        self.startup().await
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        self.shutdown().await
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        self.router_service(service)
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        self.query_planning_service(service)
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        self.execution_service(service)
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        self.subgraph_service(name, service)
    }
}

// For use when creating plugins

// Register a plugin with a name
#[macro_export]
macro_rules! register_plugin {
    ($key: literal, $value: ident) => {
        startup::on_startup! {
            // Register the plugin factory function
            apollo_router_core::plugins_mut().insert($key.to_string(), || Box::new($value::default()));
        }
    };
}
