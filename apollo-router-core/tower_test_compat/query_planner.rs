use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use apollo_router_core::{PlannedRequest, QueryPlan, RouterRequest};
use http::{Request, Response};
use tower::util::BoxCloneService;
use tower::{BoxError, Service, ServiceExt};
use typed_builder::TypedBuilder;

use crate::{graphql, Context};

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
        let query_plan = todo!();
        // create a response in a future.
        let fut = async {
            Ok(PlannedRequest {
                frontend_request: Arc::new(request.frontend_request),
                context: request.context,
            })
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}
