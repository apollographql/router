//! Implements the Execution phase of the request lifecycle.

use crate::{ExecutionRequest, ExecutionResponse, Response, SubgraphRequest, SubgraphResponse};
use crate::{Schema, ServiceRegistry};
use futures::future::BoxFuture;
use futures::stream::BoxStream;

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::BoxError;
use tower_service::Service;
use tracing::Instrument;

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

            tokio::task::spawn(
                async move {
                    req.query_plan
                        .execute(
                            &context,
                            &this.subgraph_services,
                            req.originating_request.clone(),
                            &this.schema,
                            sender,
                        )
                        .await;
                }
                .in_current_span(),
            );

            Ok(ExecutionResponse::new_from_response(
                http::Response::new(Box::pin(receiver) as BoxStream<'static, Response>).into(),
                ctx.clone(),
            ))
        }
        .in_current_span();
        Box::pin(fut)
    }
}
