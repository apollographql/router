//! Implements the Execution phase of the request lifecycle.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::StreamExt;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::BoxError;
use tower_service::Service;
use tracing::Instrument;

use crate::graphql::Response;
use crate::service_registry::ServiceRegistry;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::Schema;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

/// [`Service`] for query execution.
#[derive(Clone)]
pub struct ExecutionService {
    schema: Arc<Schema>,
    subgraph_services: Arc<ServiceRegistry>,
}

#[buildstructor::buildstructor]
impl ExecutionService {
    #[builder]
    pub fn new(
        schema: Arc<Schema>,
        subgraph_services: HashMap<
            String,
            Buffer<BoxService<SubgraphRequest, SubgraphResponse, BoxError>, SubgraphRequest>,
        >,
    ) -> Self {
        Self {
            schema,
            subgraph_services: Arc::new(ServiceRegistry::new(subgraph_services)),
        }
    }
}

impl Service<ExecutionRequest> for ExecutionService {
    type Response = ExecutionResponse<BoxStream<'static, Response>>;
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
            let ctx = context.clone();
            let (sender, receiver) = futures::channel::mpsc::channel(10);

            let first = req
                .query_plan
                .execute(
                    &context,
                    &this.subgraph_services,
                    req.originating_request.clone(),
                    &this.schema,
                    sender,
                )
                .await;

            let rest = receiver;

            let stream = once(ready(first)).chain(rest).boxed();

            Ok(ExecutionResponse::new_from_response(
                http::Response::new(stream as BoxStream<'static, Response>).into(),
                ctx,
            ))
        }
        .in_current_span();
        Box::pin(fut)
    }
}
