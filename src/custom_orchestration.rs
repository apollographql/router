use async_trait::async_trait;



use std::sync::Arc;
use crate::{Configuration, ExtensionManager, RouterFactory, Schema, ServiceRegistry, Request, Response};
use anyhow::Result;

pub struct MyCustomServiceRegistry {}

impl MyCustomServiceRegistry {
    fn new(_config: Arc<dyn Configuration>, _extensions: Arc<dyn ExtensionManager>, _schema: Arc<Schema>) -> Self {
        println!("Creating MyCustomServiceRegistry");
        MyCustomServiceRegistry {}
    }
}

#[async_trait]
impl ServiceRegistry for MyCustomServiceRegistry {
    async fn make_request(&self, _upstream_request: Request, _downstream_request: Request) -> Result<Response> {
        todo!()
    }
}

pub struct MyRouterFactory {}

#[async_trait]
impl RouterFactory for MyRouterFactory {
    //Only override the service registry in this case
    async fn create_service_registry(&mut self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, schema: Arc<Schema>) -> Result<Arc<dyn ServiceRegistry>> {
        Ok(Arc::new(MyCustomServiceRegistry::new(config.to_owned(), extensions.to_owned(), schema)))
    }
}

impl Default for MyRouterFactory {
    fn default() -> Self {
        println!("Creating MyRouterFactory");
        MyRouterFactory {}
    }
}
