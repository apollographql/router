use async_trait::async_trait;

use std::sync::Arc;
use crate::{Configuration, ExtensionManager, QueryPlanner, RoutingHandler, RouterFactory, Schema, ServiceRegistry, Request, Response};
use crate::ExtensionManagerExt;

pub struct DefaultRouterFactory {}

impl RouterFactory for DefaultRouterFactory
{}

impl Default for DefaultRouterFactory {
    fn default() -> DefaultRouterFactory {
        DefaultRouterFactory {}
    }
}

pub struct DefaultConfiguration {}

impl Default for DefaultConfiguration {
    fn default() -> Self {
        println!("Creating DefaultConfiguration");
        Self {}
    }
}

impl Configuration for DefaultConfiguration {}

pub struct DefaultSchemaLoader {}

impl DefaultSchemaLoader {
    pub async fn get(_config: Arc<dyn Configuration>, _extensions: Arc<dyn ExtensionManager>) -> Schema {
        println!("Loading schema");
        // extensions.read_schema(|| async {
        //     Schema {}
        // }).await
        Schema{}
    }
}

pub struct DefaultQueryPlanner {}

impl DefaultQueryPlanner {
    pub fn new(_config: Arc<dyn Configuration>, _extensions: Arc<dyn ExtensionManager>, _schema: Arc<Schema>) -> Self {
        println!("Creating DefaultQueryPlanner");
        DefaultQueryPlanner {}
    }
}

impl QueryPlanner for DefaultQueryPlanner {}

pub struct DefaultServiceRegistry {
    pub extensions: Arc<dyn ExtensionManager>,
}

impl DefaultServiceRegistry {
    pub fn new(_config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, _schema: Arc<Schema>) -> Self {
        println!("Creating DefaultServiceRegistry");
        DefaultServiceRegistry { extensions }
    }
}

#[async_trait]
impl ServiceRegistry for DefaultServiceRegistry {
    async fn make_request(&self, upstream_request: Request, downstream_request: Request) -> Response {
        self.extensions.make_downstream_request(upstream_request, downstream_request, |final_request| async { Response {} }).await
    }
}

pub struct DefaultRoutingHandler {}

impl DefaultRoutingHandler {
    pub fn new(_config: Arc<dyn Configuration>, _extensions: Arc<dyn ExtensionManager>, _schema: Arc<Schema>, _query_planner: Arc<dyn QueryPlanner>, _service_registry: Arc<dyn ServiceRegistry>) -> Self {
        println!("Creating DefaultRouter");
        DefaultRoutingHandler {}
    }
}

impl RoutingHandler for DefaultRoutingHandler {
    fn respond(&self) {
        println!("Responding");
    }
}
