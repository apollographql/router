use crate::prelude::graphql::*;
use futures::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct RouterService<R> {
    router: Arc<R>,
}

impl<R> Clone for RouterService<R> {
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
        }
    }
}

impl<R> RouterService<R> {
    pub fn new(router: Arc<R>) -> Self {
        Self { router }
    }
}

impl<R> tower::Service<Request> for RouterService<R>
where
    R: Router + 'static,
{
    type Response = Response;
    type Error = ();
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let router = self.router.clone();
        Box::pin(async move {
            match router.prepare_query(&request).await {
                Ok(route) => Ok(route.execute(Arc::new(request)).await),
                Err(response) => Ok(response),
            }
        })
    }
}

#[async_trait::async_trait]
impl<R> Router for RouterService<R>
where
    R: Router + 'static,
{
    type PreparedQuery = R::PreparedQuery;

    async fn prepare_query(&self, request: &Request) -> Result<Self::PreparedQuery, Response> {
        self.router.prepare_query(request).await
    }
}
