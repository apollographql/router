//! Tower fetcher for fetch node execution.

use std::pin::Pin;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;

use futures::Future;
use futures::future::FutureExt;
use tower::BoxError;
use tower::Service;

use crate::query_planner::ExecutionParameters;
use crate::services::FetchRequest;
use crate::services::FetchResponse;
use crate::services::SubgraphServiceFactory;

#[derive(Clone)]
pub(crate) struct FetchService<'a> {
    pub(crate) parameters: &'a ExecutionParameters<'a>,
}

impl<'a> FetchService<'a> {
    pub(crate) fn new(parameters: &'a ExecutionParameters<'a>) -> Result<Self, BoxError> {
        Ok(Self { parameters })
    }
}



pub(crate) struct FetchNodeFuture {
    f: Pin<Box<dyn Future<Output = FetchResponse> + 'static>>,
}

// impl FetchNodeFuture {
//     fn new(f: impl Future<Output = FetchResponse> + Unpin) -> Self {
//         Self { f: Box::pin(f) }
//     }
// }

impl Future for FetchNodeFuture
{
    type Output = Result<FetchResponse, BoxError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.f.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(v) => Poll::Ready(Ok(v))
        }
    }
}

impl<'a> tower::Service<FetchRequest<'a>> for FetchService<'a> {
    type Response = FetchResponse;
    type Error = BoxError;
    type Future = FetchNodeFuture;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: FetchRequest<'a>) -> Self::Future {
        FetchNodeFuture { f: Box::pin(request.fetch_node.fetch_node(
            request.parameters,
            request.data,
            request.current_dir,
        ))}
    }
}

#[derive(Clone)]
pub(crate) struct FetchServiceFactory {
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,
}

impl FetchServiceFactory {
    pub(crate) fn new(subgraph_service_factory: Arc<SubgraphServiceFactory>) -> Self {
        Self {
            subgraph_service_factory,
        }
    }
}
