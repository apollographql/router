use crate::{PlannedRequest, RouterRequest, RouterResponse, SubgraphRequest};
use async_trait::async_trait;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tower::util::BoxService;
use tower::BoxError;

static PLUGIN_REGISTRY: Lazy<Mutex<HashMap<String, fn() -> Box<dyn DynPlugin>>>> =
    Lazy::new(|| {
        let m = HashMap::new();
        Mutex::new(m)
    });

pub fn plugins() -> Arc<HashMap<String, fn() -> Box<dyn DynPlugin>>> {
    Arc::new(PLUGIN_REGISTRY.lock().expect("Lock poisoned").clone())
}

pub fn plugins_mut<'a>() -> MutexGuard<'a, HashMap<String, fn() -> Box<dyn DynPlugin>>> {
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
        service: BoxService<RouterRequest, PlannedRequest, BoxError>,
    ) -> BoxService<RouterRequest, PlannedRequest, BoxError> {
        service
    }

    fn execution_service(
        &mut self,
        service: BoxService<PlannedRequest, RouterResponse, BoxError>,
    ) -> BoxService<PlannedRequest, RouterResponse, BoxError> {
        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        service
    }
}

pub trait DynPlugin: Send + Sync + 'static {
    fn configure(&mut self, _configuration: &Value) -> Result<(), BoxError>;
    fn startup<'a>(&'a mut self) -> BoxFuture<'a, Result<(), BoxError>>;
    fn shutdown<'a>(&'a mut self) -> BoxFuture<'a, Result<(), BoxError>>;

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError>;

    fn query_planning_service(
        &mut self,
        service: BoxService<RouterRequest, PlannedRequest, BoxError>,
    ) -> BoxService<RouterRequest, PlannedRequest, BoxError>;

    fn execution_service(
        &mut self,
        service: BoxService<PlannedRequest, RouterResponse, BoxError>,
    ) -> BoxService<PlannedRequest, RouterResponse, BoxError>;

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError>;
}
