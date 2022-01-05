use std::collections::HashMap;
use async_trait::async_trait;
use std::error::Error;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use anyhow::Result;
use futures::future::BoxFuture;
use futures::Stream;
use tower::{Layer, Service, ServiceBuilder};
use tower::util::BoxService;


pub struct Schema;

pub struct QueryPlan;

use http::{Request, Response, StatusCode};
use tower::layer::layer_fn;
use tower::layer::util::Stack;


struct GraphQLRequest {
    //Usual stuff here
}

struct PlannedGraphQLRequest {
    // Planned request includes the original request
    request: Request<GraphQLRequest>,

    // And also the query plan
    query_plan: QueryPlan,
}

struct DownstreamGraphQLRequest {
    //The request to make downstream
    request: Request<GraphQLRequest>,

    // Downstream requests includes the original request
    upstream_request: PlannedGraphQLRequest,
}

struct GraphQLResponse {
    //Usual stuff here


    //Stream stuff here for defer/stream
    //Our warp adapter will convert the entire response to a stream if this field is present.
    stream: Option<Box<dyn Stream<Item=GraphQLPatchResponse>>>,
}

struct GraphQLPatchResponse {}

struct QueryPlannerService;

impl Service<Request<GraphQLRequest>> for QueryPlannerService {
    type Response = PlannedGraphQLRequest;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: Request<GraphQLRequest>) -> Self::Future {
        // create a response in a future.
        let fut = async {
            Ok(PlannedGraphQLRequest { request, query_plan: QueryPlan {} })
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

struct QueryExecutionService {
    query_planner_service: BoxService<Request<GraphQLRequest>, PlannedGraphQLRequest, http::Error>,
    services: Vec<BoxService<Request<GraphQLRequest>, Response<GraphQLResponse>, http::Error>>,
}

impl Service<Request<GraphQLRequest>> for QueryExecutionService {
    type Response = Response<GraphQLResponse>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: Request<GraphQLRequest>) -> Self::Future {
        todo!();
    }
}

pub struct GraphQLEndpointService {
    url: String,
}

impl Service<Request<GraphQLRequest>> for GraphQLEndpointService {
    type Response = Response<GraphQLResponse>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: Request<GraphQLRequest>) -> Self::Future {
        todo!();
    }
}


struct ApolloRouter {}

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

pub struct CacheLayer;

impl<S> Layer<S> for CacheLayer {
    type Service = S;

    fn layer(&self, service: S) -> Self::Service {
        todo!();
    }
}

pub struct PropagateHeaderLayer;

impl<S> Layer<S> for PropagateHeaderLayer {
    type Service = GraphQLEndpointService;

    fn layer(&self, service: S) -> Self::Service {
        todo!();
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

    fn propagate_header(self : ServiceBuilder<L>, header_name: &str) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>> {
        todo!()
    }
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //Query planning is a service. It take GraphQLRequest and outputs PlannedGraphQLRequest
    let mut query_planner_service = ServiceBuilder::new()
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(QueryPlannerService {});

    //Endpoint service takes a PlannedGraphQLRequest and outputs a GraphQLResponse
    let mut book_service = ServiceBuilder::new()
        .rate_limit(2, Duration::from_secs(2))
        .layer(layer_fn(|f|f)) //Custom stuff that the user wants to develop
        .service(GraphQLEndpointService { url: "http://books".to_string() });

    //Endpoint service takes a PlannedGraphQLRequest and outputs a GraphQLResponse
    let mut author_service = ServiceBuilder::new()
        .propagate_header("A")
        .cache()
        .service(GraphQLEndpointService { url: "http://authors".to_string() });

    //Execution service takes a GraphQLRequest and outputs a GraphQLResponse
    let mut query_execution_service = ServiceBuilder::new()
        .service(QueryExecutionService {
            query_planner_service: BoxService::new(query_planner_service), //Query planner service to use
            services: vec!(BoxService::new(book_service), BoxService::new(author_service)), //The list of endpoints
        });


    // User can use an adapter that we provide or embed their own or use tower-http
    query_execution_service.call(Request::new(GraphQLRequest {})).await;

    //We will provide an implementation based on Warp
    //It does hot reloading and config from yaml to build the services
    //Wasm/deno layers can be developed.
    //We can reuse what we have already developed as this slots in to where Geffroy has added Tower in:
    // https://github.com/apollographql/router/pull/293
    ApolloRouter::default().start().await;

    Ok(())
}
