//! Implements the Execution phase of the request lifecycle.

use crate::{ExecutionRequest, ExecutionResponse, SubgraphRequest, SubgraphResponse};
use crate::{Schema, ServiceRegistry};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::BoxError;
use tower_service::Service;
use tracing::Instrument;
use typed_builder::TypedBuilder;

/// [`Service`] for query execution.
#[derive(TypedBuilder, Clone)]
pub struct ExecutionService {
    schema: Arc<Schema>,

    #[builder(setter(transform = |services: HashMap<String, Buffer<BoxService<SubgraphRequest, SubgraphResponse, BoxError>, SubgraphRequest>>| Arc::new(ServiceRegistry::new(services))))]
    subgraph_services: Arc<ServiceRegistry>,
}

impl Service<ExecutionRequest> for ExecutionService {
    type Response = ExecutionResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

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

    fn call(&mut self, req: ExecutionRequest) -> Self::Future {
        let this = self.clone();
        let fut = async move {
            let context = req.context;
            let response = req
                .query_plan
                .execute(
                    &context,
                    &this.subgraph_services,
                    req.originating_request.clone(),
                    &this.schema,
                )
                .await;

            // Note that request context is not propagated from downstream.
            // Context contains a mutex for state however so in practice
            Ok(ExecutionResponse::new_from_response(
                http::Response::new(response).into(),
                context,
            ))
        }
        .in_current_span();
        Box::pin(fut)
    }
}
