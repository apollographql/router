use crate::SubgraphRequest;
use crate::{PlannedRequest, RouterResponse, Schema, ServiceRegistry};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tower::util::BoxService;
use tower::BoxError;
use tower_service::Service;
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct ExecutionService {
    schema: Arc<Schema>,

    #[builder(setter(transform = |concurrency: usize, services: HashMap<String, BoxService<SubgraphRequest, RouterResponse, BoxError>>| Arc::new(ServiceRegistry::new(concurrency, services))))]
    subgraph_services: Arc<ServiceRegistry>,
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
            let response = req
                .query_plan
                .execute(&req.context, &this.subgraph_services, &this.schema)
                .await;

            // Note that request context is not propagated from downstream.
            // Context contains a mutex for state however so in practice
            Ok(RouterResponse {
                response: http::Response::new(response),
                context: req.context,
            })
        };
        Box::pin(fut)
    }
}
