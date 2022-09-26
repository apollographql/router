//! Implements the Execution phase of the request lifecycle.

use std::future::ready;
use std::sync::Arc;
use std::task::Poll;

use futures::channel::mpsc::Receiver;
use futures::channel::mpsc::SendError;
use futures::channel::mpsc::Sender;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::SinkExt;
use futures::StreamExt;
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
use crate::services::execution;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::Schema;

/// [`Service`] for query execution.
#[derive(Clone)]
pub(crate) struct ExecutionService<SF: SubgraphServiceFactory> {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_creator: Arc<SF>,
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

            let operation_name = &req.supergraph_request.body().operation_name;
            let variables = &req.supergraph_request.body().variables;

            let is_deferred = req
                .query_plan
                .is_deferred(operation_name.as_deref(), variables);

            let first = req
                .query_plan
                .execute(
                    &context,
                    &this.subgraph_creator,
                    &Arc::new(req.supergraph_request),
                    &this.schema,
                    sender,
                )
                .await;

            let stream = if is_deferred {
                filter_stream(first, receiver).boxed()
            } else {
                once(ready(first)).chain(receiver).boxed()
            };

            Ok(ExecutionResponse::new_from_response(
                http::Response::new(stream as _),
                ctx,
            ))
        }
        .in_current_span();
        Box::pin(fut)
    }
}

// modifies the response stream to set `has_next` to `false` on the last response
fn filter_stream(first: Response, mut stream: Receiver<Response>) -> Receiver<Response> {
    let (mut sender, receiver) = futures::channel::mpsc::channel(10);

    tokio::task::spawn(async move {
        let mut seen_last_message = consume_responses(first, &mut stream, &mut sender).await?;

        while let Some(current_response) = stream.next().await {
            seen_last_message =
                consume_responses(current_response, &mut stream, &mut sender).await?;
        }

        // the response stream disconnected early so we could not add `has_next = false` to the
        // last message, so we add an empty one
        if !seen_last_message {
            sender
                .send(Response::builder().has_next(false).build())
                .await?;
        }
        Ok::<_, SendError>(())
    });

    receiver
}

// returns Ok(true) when we saw the last message
async fn consume_responses(
    mut current_response: Response,
    stream: &mut Receiver<Response>,
    sender: &mut Sender<Response>,
) -> Result<bool, SendError> {
    loop {
        match stream.try_next() {
            // no messages available, but the channel is not closed
            // this means more deferred responses can come
            Err(_) => {
                sender.send(current_response).await?;

                return Ok(false);
            }

            // there might be other deferred responses after this one,
            // so we should call `try_next` again
            Ok(Some(response)) => {
                sender.send(current_response).await?;
                current_response = response;
            }
            // the channel is closed
            // there will be no other deferred responses after that,
            // so we set `has_next` to `false`
            Ok(None) => {
                current_response.has_next = Some(false);

                sender.send(current_response).await?;
                return Ok(true);
            }
        }
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
    type Service = execution::BoxService;

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
    type ExecutionService = execution::BoxService;
    type Future = <<ExecutionCreator<SF> as NewService<ExecutionRequest>>::Service as Service<
        ExecutionRequest,
    >>::Future;
}
