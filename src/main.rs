use async_trait::async_trait;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use crate::custom_orchestration::MyRouterFactory;
use crate::default_implementations::{DefaultConfiguration, DefaultQueryPlanner, DefaultRoutingHandler, DefaultRouterFactory, DefaultSchemaLoader, DefaultServiceRegistry};
use crate::extensions::DefaultExtensionManager;

mod extensions;
mod custom_orchestration;
mod default_implementations;

pub struct Schema {}

pub struct QueryPlan {}

pub struct Request {}

impl Request {
    pub(crate) fn get_header(&self, _name: &str) -> &str {
        todo!()
    }
    pub(crate) fn set_header(&mut self, _name: &str, _value: &str) {
        todo!()
    }
}

pub struct Response {}

impl Response {
    pub(crate) fn get_header(&self, _name: &str) -> &str {
        todo!()
    }
    pub(crate) fn set_header(&mut self, _name: &str, _value: &str) {
        todo!()
    }
}

pub trait Configuration: Send + Sync {}

#[async_trait]
pub trait ServiceRegistry: Send + Sync {
    async fn make_request(&self, upstream_request: Request, downstream_request: Request) -> Response;
}

pub trait QueryPlanner: Send + Sync {}

pub trait RoutingHandler: Send + Sync {
    fn respond(&self);
}


#[async_trait]
pub trait ExtensionManager: Send + Sync {
    // async fn validate_response(&self, response: Response, delegate: Box<dyn Fn(Response)->dyn Future<Output=()> >);
    async fn do_make_downstream_request(&self, upstream_request: Request, downstream_request: Request, chain: DownstreamRequestChain) -> Response;
    // async fn plan_query(&self, request: Request, delegate: Box<dyn Fn(Request)->dyn Future<Output=QueryPlan>>) -> QueryPlan;
    //async fn do_read_schema(&self, delegate: Box<dyn Fn() -> Box<dyn Future<Output=Schema> + Send> + Send>) -> Schema;
    // async fn visit_query(&self, delegate: Box<dyn Fn()->dyn Future<Output=()>>);
}


type DownstreamRequestChain = Pin<Box<dyn Fn(Request) -> Pin<Box<dyn Future<Output=Response> + Send + Sync + 'static>> + Send + Sync + 'static>>;

#[async_trait]
trait ExtensionManagerExt: ExtensionManager {
    async fn make_downstream_request<F, T>(&self, upstream_request: Request, downstream_request: Request, f: F) -> Response
        where
            F: FnOnce(Request) -> T + Send + Sync + 'static,
            T: Future<Output=Response> + Send + Sync + 'static,
    {
        self.do_make_downstream_request(upstream_request, downstream_request, Box::pin(move |r| Box::pin(f(r)))).await
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
    async fn create_configuration(&self) -> Result<Arc<dyn Configuration>, Box<dyn Error>>
    {
        Ok(Arc::new(DefaultConfiguration::default()))
    }

    async fn create_extensions_manager(&self, config: Arc<dyn Configuration>) -> Result<Arc<dyn ExtensionManager>, Box<dyn Error>> {
        Ok(Arc::new(DefaultExtensionManager::new(config.to_owned())))
    }

    async fn create_schema(&self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>) -> Result<Schema, Box<dyn Error>> {
        Ok(DefaultSchemaLoader::get(config.to_owned(), extensions.to_owned()).await)
    }

    async fn create_query_planner(&self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, schema: Arc<Schema>) -> Result<Arc<dyn QueryPlanner>, Box<dyn Error>> {
        Ok(Arc::new(DefaultQueryPlanner::new(config.to_owned(), extensions.to_owned(), schema)))
    }

    async fn create_service_registry(&self, config: Arc<dyn Configuration>, extensions: Arc<dyn ExtensionManager>, schema: Arc<Schema>) -> Result<Arc<dyn ServiceRegistry>, Box<dyn Error>> {
        Ok(Arc::new(DefaultServiceRegistry::new(config.to_owned(), extensions.to_owned(), schema)))
    }

    async fn create_routing_handler(&self) -> Result<Box<dyn RoutingHandler>, Box<dyn Error>> {
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

    //User wants to customize one part of the router
    //Mostly it is our stuff though.
    ApolloRouter::new(MyRouterFactory::default());

    //Embed the standard router
    WarpAdapter::new(DefaultRouterFactory::default().create_routing_handler().await?);

    //Embed a customized router
    WarpAdapter::new(MyRouterFactory::default().create_routing_handler().await?);

    Ok(())
}
