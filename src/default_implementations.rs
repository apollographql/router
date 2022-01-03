
use std::mem;
use async_trait::async_trait;

use std::sync::Arc;
use crate::{Configuration, ExtensionManager, QueryPlanner, RoutingHandler, RouterFactory, Schema, ServiceRegistry, Request, Response, Extension, DownstreamRequestChain, Chain};
use crate::ExtensionManagerExt;
use anyhow::Result;

pub struct DefaultRouterFactory {
    extensions: Vec<Box<dyn Extension>>,
}

impl DefaultRouterFactory {
    pub(crate) fn with_extension<T: Extension + 'static>(mut self, extension: T) -> Self {
        self.extensions.push(Box::new(extension));
        self
    }
}


#[async_trait]
impl RouterFactory for DefaultRouterFactory
{
    async fn create_extensions_manager(&mut self, config: Arc<dyn Configuration>) -> Result<Arc<dyn ExtensionManager>> {
        let mut extensions = Vec::new();
        mem::swap(&mut self.extensions, &mut extensions);
        let manager = Arc::new(DefaultExtensionManager::new(config, Some(extensions)));
        Ok(manager)
    }
}

impl Default for DefaultRouterFactory {
    fn default() -> DefaultRouterFactory {
        DefaultRouterFactory {
            extensions: Default::default()
        }
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
        Schema {}
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
    async fn make_request(&self, upstream_request: Request, downstream_request: Request) -> Result<Response> {
        self.extensions.make_downstream_request(upstream_request, downstream_request, |final_request| async move {
            Ok(Response {
                headers: final_request.headers.clone(),
                body: "Response body".to_string()
            })
        }).await
    }
}

pub struct DefaultRoutingHandler {
    pub config: Arc<dyn Configuration>,
    pub extensions: Arc<dyn ExtensionManager>,
    pub schema: Arc<Schema>,
    pub query_planner: Arc<dyn QueryPlanner>,
    pub service_registry: Arc<dyn ServiceRegistry>,
}

impl DefaultRoutingHandler {
    pub fn new(config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, schema: Arc<Schema>, query_planner: Arc<dyn QueryPlanner>, service_registry: Arc<dyn ServiceRegistry>) -> Self {
        println!("Creating DefaultRouter");
        DefaultRoutingHandler {
            config,
            extensions,
            schema,
            query_planner,
            service_registry,
        }
    }
}

#[async_trait]
impl RoutingHandler for DefaultRoutingHandler {
    async fn respond(&self, request: Request) -> Result<Response> {
        self.service_registry.make_request(request, Request {
            headers: Default::default()
        }).await
    }
}


pub struct DefaultExtensionManager {
    extensions: Arc<Vec<Box<dyn Extension>>>,

}

impl DefaultExtensionManager {
    pub fn new(_config: Arc<dyn Configuration>, extensions: Option<Vec<Box<dyn Extension>>>) -> Self {
        println!("Creating DefaultExtensions");
        Self {
            extensions: Arc::new(extensions.unwrap_or_else(Vec::new))
        }
    }
}


#[async_trait]
impl ExtensionManager for DefaultExtensionManager {

    async fn do_make_downstream_request(&self, upstream_request: &Request, downstream_request: Request, delegate: DownstreamRequestChain) -> Result<Response> {
        if self.extensions.len() > 0 {
            self.extensions[0].make_downstream_request(Chain { extensions: self.extensions.clone(), extension_index: 0, delegate: Arc::new(delegate) }, upstream_request, downstream_request).await
        } else {
            delegate(downstream_request).await
        }
    }
}
