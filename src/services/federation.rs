use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use http::{Request, Response};
use tower::util::BoxCloneService;
use tower::{BoxError, Service, ServiceExt};
use typed_builder::TypedBuilder;

use crate::{
    graphql, Context, PlannedRequest, QueryPlan, RouterRequest, RouterResponse, SubgraphRequest,
};

#[derive(Default)]
pub struct QueryPlannerService;

impl Service<RouterRequest> for QueryPlannerService {
    type Response = PlannedRequest;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RouterRequest) -> Self::Future {
        // create a response in a future.
        let fut = async {
            Ok(PlannedRequest {
                request: request.request,
                query_plan: QueryPlan {
                    service_name: "books".to_string(), //Hard coded
                },
                context: request.context,
            })
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[derive(TypedBuilder, Clone)]
pub struct RouterService {
    query_planner_service: BoxCloneService<RouterRequest, PlannedRequest, BoxError>,
    query_execution_service: BoxCloneService<PlannedRequest, RouterResponse, BoxError>,
    #[builder(default)]
    ready_query_planner_service: Option<BoxCloneService<RouterRequest, PlannedRequest, BoxError>>,
    #[builder(default)]
    ready_query_execution_service:
        Option<BoxCloneService<PlannedRequest, RouterResponse, BoxError>>,
}

impl Service<RouterRequest> for RouterService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        if vec![
            self.ready_query_planner_service
                .get_or_insert_with(|| self.query_planner_service.clone())
                .poll_ready(cx),
            self.ready_query_execution_service
                .get_or_insert_with(|| self.query_execution_service.clone())
                .poll_ready(cx),
        ]
        .iter()
        .all(|r| r.is_ready())
        {
            return Poll::Ready(Ok(()));
        }
        Poll::Pending
    }

    fn call(&mut self, request: RouterRequest) -> Self::Future {
        let mut planning = self.ready_query_planner_service.take().unwrap();
        let mut execution = self.ready_query_execution_service.take().unwrap();
        //Here we convert to an unplanned request, this is where context gets created
        let fut = async move {
            let planned_query = planning.call(request).await;
            match planned_query {
                Ok(planned_query) => execution.call(planned_query).await,
                Err(err) => Err(err),
            }
        };

        Box::pin(fut)
    }
}

#[derive(TypedBuilder, Clone)]
pub struct ExecutionService {
    subgraph_services: HashMap<String, BoxCloneService<SubgraphRequest, RouterResponse, BoxError>>,
}

impl ExecutionService {
    fn make_request(
        context: &Context,
        service_name: &str,
        query_plan: &Arc<QueryPlan>,
        frontend_request: &Arc<Request<graphql::Request>>,
        body: &str,
    ) -> SubgraphRequest {
        SubgraphRequest {
            service_name: service_name.to_string(),
            url_override: None,
            subgraph_request: Request::new(graphql::Request {
                body: body.to_string(),
            }),
            query_plan: query_plan.clone(),
            request: frontend_request.clone(),
            context: context.clone(),
        }
    }
}

impl Service<PlannedRequest> for ExecutionService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        // We break backpressure here.
        // We can implement backpressure, but we need to think about what we want out of it.
        // For instance, should be block all services if one downstream service is not ready?
        // This may not make sense if you have hundreds of services.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: PlannedRequest) -> Self::Future {
        let this = self.clone();
        let fut = async move {
            // Fan out, context becomes immutable at this point.
            let service_name = &req.query_plan.service_name.to_string();
            let query_plan = Arc::new(req.query_plan);
            let frontend_request = Arc::new(req.request);
            let context = Arc::new(req.context);
            let req1 = Self::make_request(
                &context,
                service_name,
                &query_plan,
                &frontend_request,
                &format!("req1: {}", &frontend_request.body().body),
            );
            let req2 = Self::make_request(
                &context,
                service_name,
                &query_plan,
                &frontend_request,
                &format!("req2: {}", &frontend_request.body().body),
            );
            let mut service1 = this.subgraph_services[&req1.service_name].clone();
            let mut service2 = this.subgraph_services[&req2.service_name].clone();

            let f1 = service1.ready().await.unwrap().call(req1).await;
            let f2 = service2.ready().await.unwrap().call(req2).await;

            Ok(RouterResponse {
                request: frontend_request.clone(),
                response: Response::new(graphql::Response {
                    body: format!(
                        "{{\"{}\", \"{}\"}}",
                        f1?.response.body().body,
                        f2?.response.body().body
                    ),
                }),
                context: Default::default(),
            })
        };
        Box::pin(fut)
    }
}
