#[macro_use]
extern crate maplit;

use std::any::Any;
use std::collections::HashMap;

use std::str::FromStr;
use std::sync::Arc;

use crate::layers::cache::CacheLayer;
use crate::layers::header_propagation::PropagateHeaderLayer;
use crate::services::federation::{
    ExecutionService, QueryPlannerService, RouterService, SubgraphService,
};
use anyhow::Result;
use http::header::{HeaderName, COOKIE};
use http::{Request, Response};
use tower::layer::util::Stack;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, Service, ServiceBuilder, ServiceExt};

mod demos;
pub mod graphql;
mod layers;
mod services;

pub struct Schema;

pub struct QueryPlan {
    service_name: String,
}

#[derive(Default, Clone)]
pub struct Context {
    content: HashMap<String, Arc<dyn Any + Send + Sync>>,
}

impl Context {
    pub fn get<T: 'static>(&self, name: &str) -> Option<&T> {
        self.content.get(name).map(|d| d.downcast_ref()).flatten()
    }

    pub fn insert<T: Send + Sync + 'static>(
        &mut self,
        name: &str,
        value: T,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        self.content.insert(name.to_string(), Arc::new(value))
    }
}
pub struct RouterRequest {
    // The original request
    pub request: Request<graphql::Request>,

    pub context: Context,
}

pub struct RouterResponse {
    // The original request
    pub request: Arc<Request<graphql::Request>>,

    pub response: Response<graphql::Response>,

    pub context: Context,
}

pub struct PlannedRequest {
    // Planned request includes the original request
    pub request: Request<graphql::Request>,

    // And also the query plan
    pub query_plan: QueryPlan,

    // Cloned from RouterRequest
    pub context: Context,
}

pub struct SubgraphRequest {
    pub service_name: String,
    // The request to make downstream
    pub subgraph_request: Request<graphql::Request>,

    // And also the query plan
    pub query_plan: Arc<QueryPlan>,

    // Downstream requests includes the original request
    pub request: Arc<Request<graphql::Request>>,

    // Cloned from PlannedRequest
    pub context: Context,
}

trait ServiceBuilderExt<L> {
    //Add extra stuff here to support our needs e.g. caching
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>>;

    //This will only compile for Endpoint services
    fn propagate_header(self, header_name: &str) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>>;
    fn propagate_cookies(self) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>>;
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
        self.layer(PropagateHeaderLayer::new(
            HeaderName::from_str(header_name).unwrap(),
        ))
    }

    fn propagate_cookies(self) -> ServiceBuilder<Stack<PropagateHeaderLayer, L>> {
        self.layer(PropagateHeaderLayer::new(COOKIE))
    }
}

#[derive(Default)]
pub struct ApolloRouterBuilder {
    extensions: Vec<Box<dyn Extension>>,
    services: Vec<(
        String,
        BoxService<SubgraphRequest, RouterResponse, BoxError>,
    )>,
}

impl ApolloRouterBuilder {
    pub fn with_extension<E: Extension + 'static>(mut self, extension: E) -> ApolloRouterBuilder {
        self.extensions.push(Box::new(extension));
        self
    }

    pub fn with_service<
        S: Service<
                SubgraphRequest,
                Response = RouterResponse,
                Error = Box<(dyn std::error::Error + Send + Sync + 'static)>,
            > + Send
            + 'static,
    >(
        mut self,
        name: &str,
        service: S,
    ) -> ApolloRouterBuilder
    where
        <S as Service<SubgraphRequest>>::Future: Send,
    {
        self.services.push((name.to_string(), service.boxed()));
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

        //SubgraphService takes a SubgraphRequest and outputs a RouterResponse
        let subgraphs = Self::default_services()
            .into_iter()
            .chain(self.services.into_iter())
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

        //ExecutionService takes a PlannedRequest and outputs a RouterResponse
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
        let router_service = ServiceBuilder::new().boxed_clone().buffer(1000).service(
            self.extensions
                .iter_mut()
                .fold(
                    RouterService::builder()
                        .query_planner_service(query_planner_service)
                        .query_execution_service(execution_service)
                        .build()
                        .boxed(),
                    |acc, e| e.router_service(acc),
                )
                .map_request(|request| RouterRequest {
                    request,
                    context: Context::default(),
                })
                .map_response(|response| response.response),
        );

        ApolloRouter { router_service }
    }

    fn default_services() -> HashMap<String, BoxService<SubgraphRequest, RouterResponse, BoxError>>
    {
        //SubgraphService takes a SubgraphRequest and outputs a graphql::Response
        let book_service = ServiceBuilder::new()
            .service(SubgraphService::builder().url("http://books").build())
            .boxed();

        //SubgraphService takes a SubgraphRequest and outputs a graphql::Response
        let author_service = ServiceBuilder::new()
            .service(SubgraphService::builder().url("http://authors").build())
            .boxed();
        hashmap! {
        "books".to_string()=> book_service,
        "authors".to_string()=> author_service
        }
    }
}

pub struct ApolloRouter {
    router_service:
        BoxCloneService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
}

impl ApolloRouter {
    pub fn builder() -> ApolloRouterBuilder {
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

pub trait Extension {
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        service
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<RouterRequest, PlannedRequest, BoxError>,
    ) -> BoxService<RouterRequest, PlannedRequest, BoxError> {
        service
    }

    fn execution_service(
        &mut self,
        service: BoxService<PlannedRequest, RouterResponse, BoxError>,
    ) -> BoxService<PlannedRequest, RouterResponse, BoxError> {
        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        service
    }
}
