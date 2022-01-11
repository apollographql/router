#[macro_use]
extern crate maplit;

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use http::{Request, Response};
use tower::layer::util::{Identity, Stack};
use tower::util::{BoxCloneService, BoxLayer, BoxService};
use tower::{BoxError, Layer, Service, ServiceBuilder, ServiceExt};
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

trait ServiceBuilderExt<L> {
    //Add extra stuff here to support our needs e.g. caching
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>>;

    //This will only compile for Endpoint services
    fn propagate_header(self, header_name: &str) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>>;
}

//Demonstrate adding reusable stuff to ServiceBuilder.
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>> {
        self.layer(CacheLayer {})
    }

    fn propagate_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>> {
        self.layer(PropagateHeaderLayer::new(header_name))
    }
}

#[derive(Default)]
struct ApolloRouterBuilder {
    extensions: Vec<Box<dyn Extension>>,
}

impl ApolloRouterBuilder {
    pub fn with_extension<E: Extension + 'static>(mut self, extension: E) -> ApolloRouterBuilder {
        self.extensions.push(Box::new(extension));
        self
    }

    pub fn build(mut self) -> ApolloRouter {
        //QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let query_planner_service = ServiceBuilder::new().boxed_clone().buffer(1000).service(
            self.extensions
                .iter_mut()
                .fold(QueryPlannerService::default().boxed(), |acc, e| {
                    e.query_planning_service(acc)
                }),
        );

        //SubgraphService takes a SubgraphRequest and outputs a graphql::Response
        let subgraphs = Self::subgraph_services()
            .into_iter()
            .map(|(name, s)| {
                (
                    name.clone(),
                    ServiceBuilder::new().boxed_clone().buffer(1000).service(
                        self.extensions
                            .iter_mut()
                            .fold(s, |acc, e| e.subgraph_service(&name, acc)),
                    ),
                )
            })
            .collect();

        //ExecutionService takes a PlannedRequest and outputs a graphql::Response
        let execution_service = ServiceBuilder::new().boxed_clone().buffer(1000).service(
            self.extensions.iter_mut().fold(
                ExecutionService::builder()
                    .subgraph_services(subgraphs)
                    .build()
                    .boxed(),
                |acc, e| e.execution_service(acc),
            ),
        );

        //Router service takes a graphql::Request and outputs a graphql::Response
        let mut router_service = ServiceBuilder::new().boxed_clone().buffer(1000).service(
            self.extensions.iter_mut().fold(
                RouterService::builder()
                    .query_planner_service(query_planner_service)
                    .query_execution_service(execution_service)
                    .build()
                    .boxed(),
                |acc, e| e.router_service(acc),
            ),
        );

        ApolloRouter { router_service }
    }

    fn subgraph_services(
    ) -> HashMap<String, BoxService<SubgraphRequest, Response<graphql::Response>, BoxError>> {
        //SubgraphService takes a SubgraphRequest and outputs a graphql::Response
        let book_service = ServiceBuilder::new()
            .service(SubgraphService::builder().url("http://books").build())
            .boxed();

        //SubgraphService takes a SubgraphRequest and outputs a graphql::Response
        let author_service = ServiceBuilder::new()
            .service(SubgraphService::builder().url("http://authors").build())
            .boxed();
        hashmap! {
        "book".to_string()=> book_service,
        "author".to_string()=> author_service
        }
    }
}

struct ApolloRouter {
    router_service:
        BoxCloneService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
}

impl ApolloRouter {
    fn builder() -> ApolloRouterBuilder {
        ApolloRouterBuilder::default()
    }
}

impl ApolloRouter {
    pub async fn start(&self) {
        todo!("This will start up Warp")
    }

    //This function won't exist, but is available for demonstration
    pub async fn call(
        &self,
        request: Request<graphql::Request>,
    ) -> Result<Response<graphql::Response>, BoxError> {
        self.router_service
            .clone()
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
    }
}

trait Extension {
    fn router_service(
        &mut self,
        service: BoxService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
    ) -> BoxService<Request<graphql::Request>, Response<graphql::Response>, BoxError> {
        service
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<UnplannedRequest, PlannedRequest, BoxError>,
    ) -> BoxService<UnplannedRequest, PlannedRequest, BoxError> {
        service
    }

    fn execution_service(
        &mut self,
        service: BoxService<PlannedRequest, Response<graphql::Response>, BoxError>,
    ) -> BoxService<PlannedRequest, Response<graphql::Response>, BoxError> {
        service
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, Response<graphql::Response>, BoxError>,
    ) -> BoxService<SubgraphRequest, Response<graphql::Response>, BoxError> {
        service
    }
}

#[derive(Default)]
struct MyExtension;
impl Extension for MyExtension {
    fn router_service(
        &mut self,
        service: BoxService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
    ) -> BoxService<Request<graphql::Request>, Response<graphql::Response>, BoxError> {
        ServiceBuilder::new()
            .rate_limit(100, Duration::from_secs(2))
            .service(service)
            .map_response(|mut r| {
                r.body_mut().body = format!("Hi, {}", r.body_mut().body);
                r
            })
            .boxed()
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, Response<graphql::Response>, BoxError>,
    ) -> BoxService<SubgraphRequest, Response<graphql::Response>, BoxError> {
        if name == "book" {
            ServiceBuilder::new()
                .propagate_header("A")
                .service(service)
                .boxed()
        } else {
            ServiceBuilder::new()
                .propagate_header("B")
                .service(service)
                .boxed()
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let router = ApolloRouter::builder()
        .with_extension(MyExtension::default())
        .build();

    let response = router
        .call(
            Request::builder()
                .header("A", "HEADER_A")
                .body(graphql::Request {
                    body: "Hello1".to_string(),
                })
                .unwrap(),
        )
        .await?;
    println!("{:?}", response);
    let response = router
        .call(Request::new(graphql::Request {
            body: "Hello2".to_string(),
        }))
        .await?;
    println!("{:?}", response);

    Ok(())
}
