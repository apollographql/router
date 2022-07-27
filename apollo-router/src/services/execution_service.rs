//! Implements the Execution phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::StreamExt;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use super::layers::allow_only_http_post_mutations::AllowOnlyHttpPostMutationsLayer;
use super::new_service::NewService;
use super::subgraph_service::SubgraphServiceFactory;
use super::Plugins;
use crate::graphql::Response;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::Schema;

/// [`Service`] for query execution.
#[derive(Clone)]
pub struct ExecutionService<SF: SubgraphServiceFactory> {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_creator: Arc<SF>,
}

//#[buildstructor::buildstructor]
impl<SF: SubgraphServiceFactory> ExecutionService<SF> {
    //#[builder]
    pub fn new(schema: Arc<Schema>, subgraph_creator: Arc<SF>) -> Self {
        Self {
            schema,
            subgraph_creator,
        }
    }
}

impl<SF> Service<ExecutionRequest> for ExecutionService<SF>
where
    SF: SubgraphServiceFactory,
{
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
            let ctx = context.clone();
            let (sender, receiver) = futures::channel::mpsc::channel(10);

            let first = req
                .query_plan
                .execute(
                    &context,
                    &this.subgraph_creator,
                    &Arc::new(req.originating_request),
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

pub(crate) trait ExecutionServiceFactory:
    NewService<ExecutionRequest, Service = Self::ExecutionService> + Clone + Send + 'static
{
    type ExecutionService: Service<
            ExecutionRequest,
            Response = ExecutionResponse,
            Error = BoxError,
            Future = Self::Future,
        > + Send;
    type Future: Send;
}

#[derive(Clone)]
pub(crate) struct ExecutionCreator<SF: SubgraphServiceFactory> {
    pub(crate) schema: Arc<Schema>,
    pub(crate) plugins: Arc<Plugins>,
    pub(crate) subgraph_creator: Arc<SF>,
}

impl<SF> NewService<ExecutionRequest> for ExecutionCreator<SF>
where
    SF: SubgraphServiceFactory,
{
    type Service = BoxService<ExecutionRequest, ExecutionResponse, BoxError>;

    fn new_service(&self) -> Self::Service {
        ServiceBuilder::new()
            .layer(AllowOnlyHttpPostMutationsLayer::default())
            .service(
                self.plugins.iter().rev().fold(
                    crate::services::execution_service::ExecutionService {
                        schema: self.schema.clone(),
                        subgraph_creator: self.subgraph_creator.clone(),
                    }
                    .boxed(),
                    |acc, (_, e)| e.execution_service(acc),
                ),
            )
            .boxed()
    }
}

impl<SF: SubgraphServiceFactory> ExecutionServiceFactory for ExecutionCreator<SF> {
    type ExecutionService = BoxService<ExecutionRequest, ExecutionResponse, BoxError>;
    type Future = <<ExecutionCreator<SF> as NewService<ExecutionRequest>>::Service as Service<
        ExecutionRequest,
    >>::Future;
}
