use crate::prelude::graphql::*;
use futures::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Clone)]
pub struct RouterService<R> {
    router: Arc<R>,
}

impl<R> tower::Service<Arc<Request>> for RouterService<R>
where
    R: Router + 'static,
{
    type Response = R::PreparedQuery;
    type Error = Response;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Arc<Request>) -> Self::Future {
        let router = self.router.clone();
        Box::pin(async move { router.prepare_query(req).await })
    }
}
