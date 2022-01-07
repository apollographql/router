#[macro_use]
extern crate maplit;

use std::any::Any;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::future::FutureExt;
use futures::lock::Mutex;
use futures::Stream;
use http::header::HeaderName;
use http::{Request, Response, StatusCode};
use tower::layer::layer_fn;
use tower::layer::util::{Identity, Stack};
use tower::util::BoxService;
use tower::{BoxError, Layer, Service, ServiceBuilder};
use typed_builder::TypedBuilder;

pub struct Schema;

pub struct QueryPlan {
    service: String,
}

mod graphql {
    use std::any::Any;
    use std::collections::HashMap;

    use futures::Stream;

    #[derive(Debug)]
    pub struct Request {
        //Usual stuff here
        pub body: String,
    }

    #[derive(Debug)]
    pub struct Response {
        //Usual stuff here
        pub body: String,
        //Stream stuff here for defer/stream
        //Our warp adapter will convert the entire response to a stream if this field is present.
        //#[serde(skip_serializing)]
        //stream: Option<Box<dyn Stream<Item = Patch>>>,
    }

    pub struct Patch {}
}

pub struct Context {
    content: HashMap<String, Box<dyn Any + Send>>,
}

impl Default for Context {
    fn default() -> Self {
        Context {
            content: HashMap::new(),
        }
    }
}

impl Context {
    pub fn get<T: 'static>(&self, name: &str) -> Option<&T> {
        self.content.get(name).map(|d| d.downcast_ref()).flatten()
    }

    pub fn insert<T: Send + 'static>(
        &mut self,
        name: &str,
        value: T,
    ) -> Option<Box<dyn Any + Send>> {
        self.content.insert(name.to_string(), Box::new(value))
    }
}

struct UnplannedRequest {
    // Planned request includes the original request
    request: Request<graphql::Request>,

    context: Context,
}

struct PlannedRequest {
    // Planned request includes the original request
    request: Request<graphql::Request>,

    // And also the query plan
    query_plan: QueryPlan,

    // Cloned from UnplannedRequest
    context: Context,
}

struct SubgraphRequest {
    // The request to make downstream
    backend_request: Request<graphql::Request>,

    // And also the query plan
    query_plan: Arc<QueryPlan>,

    // Downstream requests includes the original request
    frontend_request: Arc<Request<graphql::Request>>,

    // Cloned from PlannedRequest
    context: Context,
}

struct QueryPlannerService;

impl Service<Request<graphql::Request>> for QueryPlannerService {
    type Response = PlannedRequest;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<graphql::Request>) -> Self::Future {
        // create a response in a future.
        let fut = async {
            Ok(PlannedRequest {
                request,
                query_plan: QueryPlan {
                    service: "book".to_string(), //Hard coded
                },
                context: Context::default(),
            })
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[derive(TypedBuilder)]
struct RouterService<QueryPlanner, QueryExecution>
where
    QueryPlanner:
        Service<Request<graphql::Request>, Response = PlannedRequest, Error = http::Error>,
    QueryExecution:
        Service<PlannedRequest, Response = Response<graphql::Response>, Error = http::Error>,
{
    query_planner_service: Arc<Mutex<QueryPlanner>>,
    query_execution_service: Arc<Mutex<QueryExecution>>,
}

impl<QueryPlanner, QueryExecution> Clone for RouterService<QueryPlanner, QueryExecution>
where
    QueryPlanner:
        Service<Request<graphql::Request>, Response = PlannedRequest, Error = http::Error>,
    QueryExecution:
        Service<PlannedRequest, Response = Response<graphql::Response>, Error = http::Error>,
{
    fn clone(&self) -> Self {
        Self {
            query_planner_service: self.query_planner_service.clone(),
            query_execution_service: self.query_execution_service.clone(),
        }
    }
}

impl<QueryPlanner, QueryExecution> Service<Request<graphql::Request>>
    for RouterService<QueryPlanner, QueryExecution>
where
    QueryPlanner: Service<Request<graphql::Request>, Response = PlannedRequest, Error = http::Error>
        + 'static,
    QueryExecution: Service<PlannedRequest, Response = Response<graphql::Response>, Error = http::Error>
        + 'static,
{
    type Response = Response<graphql::Response>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!();
    }

    fn call(&mut self, request: Request<graphql::Request>) -> Self::Future {
        let this = self.clone();
        let fut = async move {
            let planned_query = this.query_planner_service.lock().await.call(request).await;
            match planned_query {
                Ok(planned_query) => {
                    this.query_execution_service
                        .lock()
                        .await
                        .call(planned_query)
                        .await
                }
                Err(err) => Err(err),
            }
        };

        Box::pin(fut)
    }
}

pub struct SubgraphService {
    url: String,
}

impl Service<SubgraphRequest> for SubgraphService {
    type Response = Response<graphql::Response>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        //TODO backpressure
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let url = self.url.clone();
        let fut = async move {
            Ok(Response::new(graphql::Response {
                body: format!("{} World from {}", request.backend_request.body().body, url)
                    .to_string(),
            }))
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[derive(Debug, Clone)]
pub struct CacheLayer;

pub struct Cache<S> {
    service: S,
}

impl<S, Request> Service<Request> for Cache<S>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.service.call(req)
    }
}

impl<S> Layer<S> for CacheLayer {
    type Service = Cache<S>;

    fn layer(&self, service: S) -> Self::Service {
        Cache { service }
    }
}

pub struct PropagateHeaderLayer {
    header_name: HeaderName,
}

impl<S> Layer<S> for PropagateHeaderLayer {
    type Service = PropagateHeaderService<S>;

    fn layer(&self, service: S) -> Self::Service {
        PropagateHeaderService {
            service,
            header_name: self.header_name.to_owned(),
        }
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

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut request: SubgraphRequest) -> Self::Future {
        //Add the header to the request and pass it on to the service.
        if let Some(header) = request.frontend_request.headers().get(&self.header_name) {
            request
                .backend_request
                .headers_mut()
                .insert(self.header_name.to_owned(), header.clone());
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
        self.layer(CacheLayer {})
    }

    fn propagate_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>> {
        self.layer(PropagateHeaderLayer {
            header_name: HeaderName::from_str(header_name).unwrap(),
        })
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

struct ExecutionService {
    subgraphs: Arc<
        Mutex<
            HashMap<String, BoxService<SubgraphRequest, Response<graphql::Response>, http::Error>>,
        >,
    >,
}

impl Clone for ExecutionService {
    fn clone(&self) -> Self {
        Self {
            subgraphs: self.subgraphs.clone(),
        }
    }
}

impl Service<PlannedRequest> for ExecutionService {
    type Response = Response<graphql::Response>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn call(&mut self, req: PlannedRequest) -> Self::Future {
        let this = self.clone();
        let fut = async move {
            let query_plan = Arc::new(req.query_plan);
            let frontend_request = Arc::new(req.request);
            let f1 = this
                .subgraphs
                .lock()
                .await
                .get_mut(&query_plan.service)
                .unwrap()
                .call(SubgraphRequest {
                    backend_request: Request::new(graphql::Request {
                        body: format!("{}{}", &frontend_request.body().body, "-Subrequest1")
                            .to_string(),
                    }),
                    query_plan: query_plan.clone(),
                    frontend_request: frontend_request.clone(),
                    context: Default::default(),
                })
                .await;
            let f2 = this
                .subgraphs
                .lock()
                .await
                .get_mut(&query_plan.service)
                .unwrap()
                .call(SubgraphRequest {
                    backend_request: Request::new(graphql::Request {
                        body: format!("{}{}", &frontend_request.body().body, "-Subrequest2")
                            .to_string(),
                    }),
                    query_plan: query_plan.clone(),
                    frontend_request: frontend_request.clone(),
                    context: Default::default(),
                })
                .await;
            Ok(Response::new(graphql::Response {
                body: format!(
                    "sg1:{} sg2:{}",
                    f1.unwrap().body().body,
                    f2.unwrap().body().body
                ),
            }))
        };
        Box::pin(fut)
    }
}

struct Configuration {}

trait Extension {
    fn configure(configuration: Configuration);
}

struct DynamicExtensionsLayer {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //Query planning is a service. It take graphql::Request and outputs Plannedgraphql::Request
    let mut query_planner_service = ServiceBuilder::new()
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(QueryPlannerService {});

    //Endpoint service takes a Downstreamgraphql::Request and outputs a graphql::Response
    let mut book_service = ServiceBuilder::new()
        .rate_limit(2, Duration::from_secs(2))
        .service(SubgraphService {
            url: "http://books".to_string(),
        });

    //Endpoint service takes a Downstreamgraphql::Request and outputs a graphql::Response
    let mut author_service =
        ServiceBuilder::new()
            .propagate_header("A")
            .cache()
            .service(SubgraphService {
                url: "http://authors".to_string(),
            });

    let mut execution_service = ServiceBuilder::new()
        .cache()
        .rate_limit(2, Duration::from_secs(10))
        .service(ExecutionService {
            subgraphs: Arc::new(Mutex::new(hashmap! {
            "book".to_string()=> BoxService::new(book_service),
            "author".to_string()=> BoxService::new(author_service)
            })),
        });

    //Execution service takes a graphql::Request and outputs a graphql::Response
    let mut router_service = ServiceBuilder::new()
        .timeout(Duration::from_secs(1))
        .service(
            RouterService::builder()
                .query_planner_service(Arc::new(Mutex::new(query_planner_service)))
                .query_execution_service(Arc::new(Mutex::new(execution_service)))
                .build(),
        );

    // User can use an adapter that we provide or embed their own or use tower-http
    match router_service
        .call(Request::new(graphql::Request {
            body: "Hello".to_string(),
        }))
        .await
    {
        Ok(response) => {
            println!("{:?}", response);
        }
        Err(error) => {
            println!("{}", error);
        }
    }

    //We will provide an implementation based on Warp
    //It does hot reloading and config from yaml to build the services
    //Wasm/deno layers can be developed.
    //We can reuse what we have already developed as this slots in to where Geoffroy has added Tower in:
    // https://github.com/apollographql/router/pull/293
    //  ApolloRouter::builder().build().start().await;

    Ok(())
}
