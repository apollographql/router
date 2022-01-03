use std::collections::HashMap;
use async_trait::async_trait;
use std::error::Error;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use crate::custom_orchestration::MyRouterFactory;
use crate::default_implementations::{DefaultConfiguration, DefaultQueryPlanner, DefaultRoutingHandler, DefaultRouterFactory, DefaultSchemaLoader, DefaultServiceRegistry, DefaultExtensionManager};
use anyhow::Result;
use crate::extensions::{HeadersExtension, SecurityExtension};

mod extensions;
mod custom_orchestration;
mod default_implementations;

pub struct Schema;

pub struct QueryPlan;

#[derive(Debug, Clone)]
pub struct Request {
    headers: HashMap<String, String>,
}


impl Request {
    pub(crate) fn get_header(&self, name: &str) -> Option<&String> {
        self.headers.get(name)
    }
    pub(crate) fn set_header(&mut self, name: &str, value: &str) -> Option<String> {
        self.headers.insert(name.to_string(), value.to_string())
    }
}

#[derive(Debug)]
pub struct Response {
    headers: HashMap<String, String>,
    body: String
}

impl Response {
    pub(crate) fn get_header(&self, name: &str) -> Option<&String> {
        self.headers.get(name)
    }
    pub(crate) fn set_header(&mut self, name: &str, value: &str) -> Option<String> {
        self.headers.insert(name.to_string(), value.to_string())
    }
}

pub trait Configuration: Send + Sync {}

#[async_trait]
pub trait ServiceRegistry: Send + Sync {
    async fn make_request(&self, upstream_request: Request, downstream_request: Request) -> Result<Response>;
}

pub trait QueryPlanner: Send + Sync {}

#[async_trait]
pub trait RoutingHandler: Send + Sync {
    async fn respond(&self, request: Request) -> Result<Response>;
}


#[async_trait]
pub trait ExtensionManager: Send + Sync {

    // async fn validate_response(&self, response: Response, delegate: Box<dyn Fn(Response)->dyn Future<Output=()> >);
    async fn do_make_downstream_request(&self, upstream_request: &Request, downstream_request: Request, chain: DownstreamRequestChain) -> Result<Response>;
    // async fn plan_query(&self, request: Request, delegate: Box<dyn Fn(Request)->dyn Future<Output=QueryPlan>>) -> QueryPlan;
    //async fn do_read_schema(&self, delegate: Box<dyn Fn() -> Box<dyn Future<Output=Schema> + Send> + Send>) -> Schema;
    // async fn visit_query(&self, delegate: Box<dyn Fn()->dyn Future<Output=()>>);
}


type DownstreamRequestChain = Box<dyn Fn(Request) -> Pin<Box<dyn Future<Output=Result<Response>> + Send + Sync + 'static>> + Send + Sync + 'static>;

// This trait allows the user to supply a lambda when calling an extension on ExtensionManager
#[async_trait]
pub trait ExtensionManagerExt: ExtensionManager {
    async fn make_downstream_request<F, T>(&self, upstream_request: Request, downstream_request: Request, f: F) -> Result<Response>
        where
            F: Fn(Request) -> T + Send + Sync + 'static,
            T: Future<Output=Result<Response>> + Send + Sync + 'static,
    {
        self.do_make_downstream_request(&upstream_request, downstream_request, Box::new(move |r| Box::pin(f(r)))).await
    }
}

pub struct Chain {
    extension_index: usize,
    extensions: Arc<Vec<Box<dyn Extension>>>,
    delegate: Arc<DownstreamRequestChain>,
}

impl Chain {
    pub async fn validate_response(&self, _response: Response) -> Result<Response>{
        todo!()
    }
    pub async fn make_downstream_request(&self, upstream_request: &Request, downstream_request: Request) -> Result<Response> {
        if self.extensions.len() == self.extension_index {
            (self.delegate)(downstream_request).await
        } else {
            let current_extension = &mut self.extensions.get(self.extension_index).unwrap();
            let next = Chain {
                extension_index: self.extension_index + 1,
                extensions: self.extensions.clone(),
                delegate: self.delegate.clone(),
            };
            current_extension.make_downstream_request(next, upstream_request, downstream_request).await
        }
    }


    pub async fn plan_query(&self, _request: Request) -> Result<QueryPlan> {
        todo!();
    }
    pub async fn schema_read(&self) -> Result<Schema> {
        todo!();
    }


}


#[async_trait]
pub trait Extension: Send + Sync {
    async fn configure(&self, _configuration: Arc<dyn Configuration>) {}
    async fn schema_read(&self, chain: Chain) -> Result<Schema> {
        chain.schema_read().await
    }
    async fn plan_query(&self, chain: Chain, upstream_request: Request) -> Result<QueryPlan> {
        chain.plan_query(upstream_request).await
    }
    async fn make_downstream_request(&self, chain: Chain, upstream_request: &Request, downstream_request: Request) -> Result<Response> {
        chain.make_downstream_request(upstream_request, downstream_request).await
    }
    async fn validate_response(&self, chain: Chain, response: Response) -> Result<Response>{
        chain.validate_response(response).await
    }
}

impl ExtensionManagerExt for dyn ExtensionManager {}


struct ApolloRouter {
    router_factory: Box<dyn RouterFactory>,
}


impl ApolloRouter
{
    fn new(router_factory: impl RouterFactory + 'static) -> ApolloRouter {
        Self {
            router_factory: Box::new(router_factory)
        }
    }
}

impl Default for ApolloRouter {
    fn default() -> Self {
        ApolloRouter::new(DefaultRouterFactory::default())
    }
}

#[async_trait]
trait RouterFactory: Send + Sync {
    async fn create_configuration(&mut self) -> Result<Arc<dyn Configuration>, Box<dyn Error>>
    {
        Ok(Arc::new(DefaultConfiguration::default()))
    }

    async fn create_extensions_manager(&mut self, config: Arc<dyn Configuration>) -> Result<Arc<dyn ExtensionManager>> {
        Ok(Arc::new(DefaultExtensionManager::new(config.to_owned(), None)))
    }

    async fn create_schema(&mut self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>) -> Result<Schema> {
        Ok(DefaultSchemaLoader::get(config.to_owned(), extensions.to_owned()).await)
    }

    async fn create_query_planner(&mut self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, schema: Arc<Schema>) -> Result<Arc<dyn QueryPlanner>> {
        Ok(Arc::new(DefaultQueryPlanner::new(config.to_owned(), extensions.to_owned(), schema)))
    }

    async fn create_service_registry(&mut self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, schema: Arc<Schema>) -> Result<Arc<dyn ServiceRegistry>> {
        Ok(Arc::new(DefaultServiceRegistry::new(config.to_owned(), extensions.to_owned(), schema)))
    }

    async fn create_routing_handler(&mut self) -> Result<Box<dyn RoutingHandler>, Box<dyn Error>> {
        //Basic DI
        //We do all the arc cloning so that users don't have to.
        let config = self.create_configuration().await?;
        let extensions_manager = self.create_extensions_manager(config.to_owned()).await?;
        let schema = Arc::new(self.create_schema(config.to_owned(), extensions_manager.to_owned()).await?);
        let query_planner = self.create_query_planner(config.to_owned(), extensions_manager.to_owned(), schema.to_owned()).await?;
        let service_registry = self.create_service_registry(config.to_owned(), extensions_manager.to_owned(), schema.to_owned()).await?;
        Ok(Box::new(DefaultRoutingHandler::new(config, extensions_manager, schema, query_planner, service_registry)))
    }
}

struct WarpAdapter {}

impl WarpAdapter {
    fn new(_handler: Box<dyn RoutingHandler>) {}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //The thing that most people will use
    //It internally has warp, yaml config, etc.
    ApolloRouter::default();

    //User wants to customize one part of the router (see custom_orchestration.rs)
    //Mostly it is our stuff though.
    ApolloRouter::new(MyRouterFactory::default());

    //Embed the standard router
    WarpAdapter::new(DefaultRouterFactory::default().create_routing_handler().await?);

    //Embed a customized router (see custom_orchestration.rs)
    WarpAdapter::new(MyRouterFactory::default().create_routing_handler().await?);



    ////////////////////////////////////////
    // Demonstrate extensions (see extensions.rs)
    // Note that extensions are expected to be interior mutable!!!!!!!
    // This is not because I like this, it seems like a necessary tradeoff to prevent having to wrap every extension in a mutex.
    // Maybe there is a better way?
    //
    // Extensions will generally be configured dynamically through config, but here we demonstrate programmatic config.
    let routing_handler = DefaultRouterFactory::default()
        .with_extension(HeadersExtension{})
        .with_extension(SecurityExtension{})
        .create_routing_handler().await?;

    //Demonstrate header propagation extension
    let mut custom_headers = HashMap::new();
    custom_headers.insert("A".to_string(), "HEADER A".to_string());
    let response = routing_handler.respond(Request{
        headers: custom_headers
    }).await?;
    println!("{:?}", response);

    //Demonstrate security extension
    let response = routing_handler.respond(Request{
        headers: Default::default()
    }).await?;
    println!("{:?}", response);


    Ok(())
}
