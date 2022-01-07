#[macro_use]
extern crate maplit;

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::Stream;
use http::{Request, Response, StatusCode};
use http::header::HeaderName;
use tower::{Layer, Service, ServiceBuilder};
use tower::layer::layer_fn;
use tower::layer::util::Stack;
use tower::util::BoxService;
use typed_builder::TypedBuilder;

pub struct Schema;

pub struct QueryPlan;

mod graphql {
    use futures::Stream;

    pub struct Request {
        //Usual stuff here
    }
    pub struct Response {
        //Usual stuff here


        //Stream stuff here for defer/stream
        //Our warp adapter will convert the entire response to a stream if this field is present.
        //#[serde(skip_serializing)]
        stream: Option<Box<dyn Stream<Item=Patch>>>,
    }


    pub struct Patch {}


}


struct PlannedRequest {
    // Planned request includes the original request
    request: Request<graphql::Request>,

    // And also the query plan
    query_plan: QueryPlan,
}

struct SubgraphRequest {
    //The request to make downstream
    backend_request: Request<graphql::Request>,

    // Downstream requests includes the original request
    frontend_request: Arc<PlannedRequest>,
}


struct QueryPlannerService;



impl Service<Request<graphql::Request>> for QueryPlannerService {
    type Response = PlannedRequest;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: Request<graphql::Request>) -> Self::Future {
        // create a response in a future.
        let fut = async {
            Ok(PlannedRequest { request, query_plan: QueryPlan {} })
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[derive(TypedBuilder)]
struct RouterService<QueryPlanner, QueryExecution> {
    query_planner_service: QueryPlanner,
    query_execution_service: QueryExecution,
}

impl<QueryPlanner, QueryExecution> Service<Request<graphql::Request>> for RouterService<QueryPlanner, QueryExecution>
    where
        QueryPlanner: Service<Request<graphql::Request>, Response=PlannedRequest, Error=http::Error>,
        QueryExecution: Service<PlannedRequest, Response=Response<graphql::Response>, Error=http::Error>
{
    type Response = Response<graphql::Response>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: Request<graphql::Request>) -> Self::Future {
        todo!();
    }
}

pub struct SubgraphService {
    url: String,
}

impl Service<SubgraphRequest> for SubgraphService {
    type Response = Response<graphql::Response>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        todo!();
    }
}


pub struct CacheLayer;

impl<S> Layer<S> for CacheLayer {
    type Service = S;

    fn layer(&self, service: S) -> Self::Service {
        todo!();
    }
}

pub struct PropagateHeaderLayer {
    header_name: HeaderName,
}

impl<S> Layer<S> for PropagateHeaderLayer {
    type Service = PropagateHeaderService<S>;

    fn layer(&self, service: S) -> Self::Service {
        PropagateHeaderService { service, header_name: self.header_name.to_owned() }
    }
}


pub struct PropagateHeaderService<S> {
    service: S,
    header_name: HeaderName,
}

impl<S> Service<SubgraphRequest> for PropagateHeaderService<S>
    where
        S: Service<SubgraphRequest>,

{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, mut request: SubgraphRequest) -> Self::Future {
        //Add the header to the request and pass it on to the service.
        if let Some(header) = request.frontend_request.request.headers().get(&self.header_name) {
            request.backend_request.headers_mut().insert(self.header_name.to_owned(), header.clone());
        }
        self.service.call(request)
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
        todo!();
    }

    fn propagate_header(self: ServiceBuilder<L>, header_name: &str) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>> {
        self.layer(PropagateHeaderLayer { header_name: HeaderName::from_str(header_name).unwrap() })
    }
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

impl Default for ApolloRouter {
    fn default() -> Self {
        todo!()
    }
}


struct ExecutionService {
    subgraphs: HashMap<String, BoxService<SubgraphRequest, Response<graphql::Response>, http::Error>>,
}

impl Service<PlannedRequest> for ExecutionService {
    type Response = Response<graphql::Response>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        todo!()
    }

    fn call(&mut self, req: PlannedRequest) -> Self::Future {
        todo!()
    }
}

struct Configuration {

}

trait Extension {
    fn configure(configuration: Configuration);
}

struct DynamicExtensionsLayer {

}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    let dynamic_extensions = DynamicExtensions::default();

    //Query planning is a service. It take graphql::Request and outputs Plannedgraphql::Request
    let mut query_planner_service = ServiceBuilder::new()
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(QueryPlannerService {});

    //Endpoint service takes a Downstreamgraphql::Request and outputs a graphql::Response
    let mut book_service = ServiceBuilder::new()
        .rate_limit(2, Duration::from_secs(2))
        .layer(layer_fn(|f| f)) //Custom stuff that the user wants to develop
        .service(SubgraphService { url: "http://books".to_string() });

    //Endpoint service takes a Downstreamgraphql::Request and outputs a graphql::Response
    let mut author_service = ServiceBuilder::new()
        .propagate_header("A")
        .cache()
        .service(SubgraphService { url: "http://authors".to_string() });

    let mut execution_service = ServiceBuilder::new()
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(ExecutionService {
            subgraphs: hashmap! {
            "book".to_string()=> BoxService::new(book_service),
            "author".to_string()=> BoxService::new(author_service)
            }
        });


    //Execution service takes a graphql::Request and outputs a graphql::Response
    let mut router_service = RouterService::builder()
        .query_planner_service(query_planner_service)
        .query_execution_service(execution_service).build();

    // User can use an adapter that we provide or embed their own or use tower-http
    router_service.call(Request::new(graphql::Request {})).await;

    //We will provide an implementation based on Warp
    //It does hot reloading and config from yaml to build the services
    //Wasm/deno layers can be developed.
    //We can reuse what we have already developed as this slots in to where Geoffroy has added Tower in:
    // https://github.com/apollographql/router/pull/293
    ApolloRouter::builder()
        .extensions()
        .build()
        .start().await;


    Ok(())
}
