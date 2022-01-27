use crate::traits::QueryPlanner;
use crate::{PlannedRequest, QueryPlanOptions, RouterBridgeQueryPlanner, RouterRequest};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tower::BoxError;
use tower_service::Service;

impl Service<RouterRequest> for RouterBridgeQueryPlanner {
    type Response = PlannedRequest;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RouterRequest) -> Self::Future {
        let this = self.clone();
        let fut = async move {
            let body = request.http_request.body();
            match this
                .get(
                    body.query.to_owned(),
                    body.operation_name.to_owned(),
                    QueryPlanOptions::default(),
                )
                .await
            {
                Ok(query_plan) => Ok(PlannedRequest {
                    query_plan,
                    context: request.context.with_request(Arc::new(request.http_request)),
                }),
                Err(e) => Err(BoxError::from(e)),
            }
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}
