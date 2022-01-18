#[macro_use]
extern crate maplit;

use std::any::Any;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use http::header::{HeaderName, COOKIE};
use http::{HeaderValue, Request, Response, Uri};
use tower::layer::util::Stack;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, Service, ServiceBuilder, ServiceExt};
use tracing::Span;

use crate::layers::cache::CacheLayer;
use crate::layers::header_manipulation::{HeaderManipulationLayer, Operation};
use crate::layers::instrument::InstrumentLayer;
use crate::services::federation::{ExecutionService, QueryPlannerService, RouterService};
use crate::services::graphql_subgraph_service::GraphQlSubgraphService;

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

    //Set this to override the URL of the service
    pub url_override: Option<Uri>,

    // The request to make downstream
    pub subgraph_request: Request<graphql::Request>,

    // And also the query plan
    pub query_plan: Arc<QueryPlan>,

    // Downstream requests includes the original request
    pub request: Arc<Request<graphql::Request>>,

    // Cloned from PlannedRequest
    pub context: Context,
}

pub trait ServiceBuilderExt<L> {
    //Add extra stuff here to support our needs e.g. caching
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>>;

    //This will only compile for Endpoint services
    fn propagate_all_headers(self) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn propagate_header(
        self,
        header_name: &str,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn propagate_or_default_header(
        self,
        header_name: &str,
        value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn remove_header(self, header_name: &str) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn insert_header(
        self,
        header_name: &str,
        value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn propagate_cookies(self) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;

    fn instrument<Request, FnType: Fn(&Request) -> Span>(
        self,
        fn_span: FnType,
    ) -> ServiceBuilder<Stack<InstrumentLayer<Request, FnType>, L>>;
}

//Demonstrate adding reusable stuff to ServiceBuilder.
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn cache(self) -> ServiceBuilder<Stack<CacheLayer, L>> {
        self.layer(CacheLayer {})
    }

    fn propagate_all_headers(
        self: ServiceBuilder<L>,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::new(Operation::PropagateAll))
    }

    fn propagate_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::new(Operation::Propagate(
            HeaderName::from_str(header_name).unwrap(),
        )))
    }

    fn propagate_or_default_header(
        self: ServiceBuilder<L>,
        header_name: &str,
        default_header_value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::new(Operation::PropagateOrDefault(
            HeaderName::from_str(header_name).unwrap(),
            default_header_value,
        )))
    }

    fn insert_header(
        self: ServiceBuilder<L>,
        header_name: &str,
        header_value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::new(Operation::Insert(
            HeaderName::from_str(header_name).unwrap(),
            header_value,
        )))
    }

    fn remove_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::new(Operation::Remove(
            HeaderName::from_str(header_name).unwrap(),
        )))
    }

    fn propagate_cookies(self) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::new(Operation::Propagate(COOKIE)))
    }

    fn instrument<Request, FnType: Fn(&Request) -> Span>(
        self,
        fn_span: FnType,
    ) -> ServiceBuilder<Stack<InstrumentLayer<Request, FnType>, L>> {
        self.layer(InstrumentLayer::new(fn_span))
    }
}

#[derive(Default)]
pub struct ApolloRouterBuilder {
    plugins: Vec<Box<dyn Plugin>>,
    services: Vec<(
        String,
        BoxService<SubgraphRequest, RouterResponse, BoxError>,
    )>,
}

impl ApolloRouterBuilder {
    pub fn with_plugin<E: Plugin + 'static>(mut self, plugin: E) -> ApolloRouterBuilder {
        self.plugins.push(Box::new(plugin));
        self
    }

    pub fn with_subgraph_service<
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
            self.plugins
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
                        self.plugins
                            .iter_mut()
                            .fold(s, |acc, e| e.subgraph_service(&name, acc)),
                    ),
                )
            })
            .collect();

        //ExecutionService takes a PlannedRequest and outputs a RouterResponse
        let execution_service = ServiceBuilder::new().boxed_clone().buffer(1000).service(
            self.plugins.iter_mut().fold(
                ExecutionService::builder()
                    .subgraph_services(subgraphs)
                    .build()
                    .boxed(),
                |acc, e| e.execution_service(acc),
            ),
        );

        //Router service takes a graphql::Request and outputs a graphql::Response
        let router_service = ServiceBuilder::new().boxed_clone().buffer(1000).service(
            self.plugins
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
            .service(
                GraphQlSubgraphService::builder()
                    .url(Uri::from_str("http://books").unwrap())
                    .build(),
            )
            .boxed();

        //SubgraphService takes a SubgraphRequest and outputs a graphql::Response
        let author_service = ServiceBuilder::new()
            .service(
                GraphQlSubgraphService::builder()
                    .url(Uri::from_str("http://authors").unwrap())
                    .build(),
            )
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

pub trait Plugin {
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

#[cfg(feature = "plugin_tests")]
pub mod test_utils {

    use super::*;
    use std::borrow::Cow;

    pub trait TestablePlugin {
        fn before_router(&self, router_request: RouterRequest) -> RouterRequest {
            router_request
        }
        fn after_router(&self, router_response: RouterResponse) -> RouterResponse {
            router_response
        }

        fn before_query_planning(&self, router_request: RouterRequest) -> RouterRequest {
            router_request
        }

        fn after_query_planning(&self, planned_request: PlannedRequest) -> PlannedRequest {
            planned_request
        }

        fn before_execution(&self, planned_request: PlannedRequest) -> PlannedRequest {
            planned_request
        }

        fn after_execution(&self, router_response: RouterResponse) -> RouterResponse {
            router_response
        }

        fn before_subgraph(
            &self,
            _name: &str,
            subgraph_request: SubgraphRequest,
        ) -> SubgraphRequest {
            subgraph_request
        }
        fn after_subgraph(&self, _name: &str, router_response: RouterResponse) -> RouterResponse {
            router_response
        }
    }

    impl<Test> Plugin for Test
    where
        Test: TestablePlugin + Send + Sync + Clone + 'static,
    {
        fn router_service(
            &mut self,
            service: BoxService<RouterRequest, RouterResponse, BoxError>,
        ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
            let clone_for_before = self.clone();
            let clone_for_after = self.clone();
            ServiceBuilder::new()
                .map_request(move |request| clone_for_before.before_router(request))
                .map_response(move |response| clone_for_after.after_router(response))
                .service(service)
                .boxed()
        }

        fn query_planning_service(
            &mut self,
            service: BoxService<RouterRequest, PlannedRequest, BoxError>,
        ) -> BoxService<RouterRequest, PlannedRequest, BoxError> {
            let clone_for_before = self.clone();
            let clone_for_after = self.clone();
            ServiceBuilder::new()
                .map_request(move |request| clone_for_before.before_query_planning(request))
                .map_response(move |planned_request| {
                    clone_for_after.after_query_planning(planned_request)
                })
                .service(service)
                .boxed()
        }

        fn execution_service(
            &mut self,
            service: BoxService<PlannedRequest, RouterResponse, BoxError>,
        ) -> BoxService<PlannedRequest, RouterResponse, BoxError> {
            let clone_for_before = self.clone();
            let clone_for_after = self.clone();
            ServiceBuilder::new()
                .map_request(move |planned_request| {
                    clone_for_before.before_execution(planned_request)
                })
                .map_response(move |router_response| {
                    clone_for_after.after_execution(router_response)
                })
                .service(service)
                .boxed()
        }

        fn subgraph_service(
            &mut self,
            service_name: &str,
            service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
        ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
            let before_name = Cow::from(service_name.to_string());
            let after_name = before_name.clone();

            let clone_for_before = self.clone();
            let clone_for_after = self.clone();
            ServiceBuilder::new()
                .map_request(move |subgraph_request| {
                    clone_for_before.before_subgraph(&before_name, subgraph_request)
                })
                .map_response(move |router_response| {
                    clone_for_after.after_subgraph(&after_name, router_response)
                })
                .service(service)
                .boxed()
        }
    }
}
