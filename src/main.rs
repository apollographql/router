#[macro_use]
extern crate maplit;

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use http::Request;
use tower::layer::util::Stack;
use tower::{BoxError, Service, ServiceBuilder, ServiceExt};
use typed_builder::TypedBuilder;

use crate::cache::CacheLayer;
use crate::federation::{ExecutionService, QueryPlannerService, RouterService, SubgraphService};
use crate::header_propagation::PropagateHeaderLayer;

mod cache;
mod federation;
mod header_propagation;

pub struct Schema;

pub struct QueryPlan {
    service_name: String,
}

mod graphql {

    #[derive(Debug)]
    pub struct Request {
        //Usual stuff here
        pub body: String,
    }

    #[derive(Debug)]
    pub struct Response {
        //Usual stuff here
        pub body: String,
    }
}

#[derive(Default)]
pub struct Context {
    content: HashMap<String, Box<dyn Any + Send + Sync>>,
}

impl Context {
    pub fn get<T: 'static>(&self, name: &str) -> Option<&T> {
        self.content.get(name).map(|d| d.downcast_ref()).flatten()
    }

    pub fn insert<T: Send + Sync + 'static>(
        &mut self,
        name: &str,
        value: T,
    ) -> Option<Box<dyn Any + Send + Sync>> {
        self.content.insert(name.to_string(), Box::new(value))
    }
}
pub struct UnplannedRequest {
    // The original request
    pub request: Request<graphql::Request>,

    pub context: Context,
}

pub struct PlannedRequest {
    // Planned request includes the original request
    pub request: Request<graphql::Request>,

    // And also the query plan
    pub query_plan: QueryPlan,

    // Cloned from UnplannedRequest
    pub context: Context,
}

pub struct SubgraphRequest {
    pub service_name: String,
    // The request to make downstream
    pub backend_request: Request<graphql::Request>,

    // And also the query plan
    pub query_plan: Arc<QueryPlan>,

    // Downstream requests includes the original request
    pub frontend_request: Arc<Request<graphql::Request>>,

    // Cloned from PlannedRequest
    pub context: Arc<Context>,
}

#[derive(TypedBuilder)]
struct ApolloRouter {
    //extensions: Vec<Box<dyn Extension>>,
}

impl ApolloRouter {
    pub(crate) async fn start(&self) {
        todo!()
    }
}

trait ServiceBuilderExt<L> {
    //Add extra stuff here to support our needs e.g. caching
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>>;

    //This will only compile for Endpoint services
    fn propagate_header(self, header_name: &str) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>>;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>> {
        //Implement our caching stuff here
        self.layer(CacheLayer {})
    }

    fn propagate_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>> {
        self.layer(PropagateHeaderLayer::new(header_name))
    }
}

struct Configuration {}

trait Extension {
    fn configure(configuration: Configuration);
}

struct DynamicExtensionsLayer {}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    //Query planning is a service. It take graphql::Request and outputs Plannedgraphql::Request
    let query_planner_service = ServiceBuilder::new()
        .boxed_clone()
        .buffer(1000)
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(QueryPlannerService::default());

    //Endpoint service takes a Downstreamgraphql::Request and outputs a graphql::Response
    let book_service = ServiceBuilder::new()
        .boxed_clone()
        .buffer(1000)
        .rate_limit(2, Duration::from_secs(2))
        .service(
            SubgraphService::builder()
                .url("http://books".to_string())
                .build(),
        );

    //Endpoint service takes a Downstreamgraphql::Request and outputs a graphql::Response
    let author_service = ServiceBuilder::new()
        .boxed_clone()
        .buffer(1000)
        .propagate_header("A")
        .cache()
        .service(
            SubgraphService::builder()
                .url("http://authors".to_string())
                .build(),
        );

    let execution_service = ServiceBuilder::new()
        .boxed_clone()
        .buffer(1000)
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(
            ExecutionService::builder()
                .subgraph_services(hashmap! {
                "book".to_string()=> book_service,
                "author".to_string()=> author_service
                })
                .build(),
        );

    //Execution service takes a graphql::Request and outputs a graphql::Response
    let mut router_service = ServiceBuilder::new()
        .timeout(Duration::from_secs(1))
        .service(
            RouterService::builder()
                .query_planner_service(query_planner_service)
                .query_execution_service(execution_service)
                .ready_query_execution_service(None)
                .ready_query_planner_service(None)
                .build(),
        );

    // User can use an adapter that we provide or embed their own or use tower-http
    let response = router_service
        .ready()
        .await?
        .call(Request::new(graphql::Request {
            body: "Hello".to_string(),
        }))
        .await?;
    println!("{:?}", response);

    Ok(())
}
