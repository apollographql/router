use crate::prelude::graphql::*;
use futures::future::BoxFuture;
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

    pub fn into_inner(self) -> Arc<R> {
        self.router
    }
}

impl<R> tower::Service<RouterRequest> for RouterService<R>
where
    R: Router + 'static,
{
    type Response = Response;
    type Error = ();
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RouterRequest) -> Self::Future {
        let router = self.router.clone();
        Box::pin(async move {
            match router.prepare_query(request.frontend_request.body()).await {
                Ok(route) => Ok(route.execute(request).await),
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

#[derive(Debug)]
pub struct FetcherService<F> {
    fetcher: Arc<F>,
}

impl<F> Clone for FetcherService<F> {
    fn clone(&self) -> Self {
        Self {
            fetcher: self.fetcher.clone(),
        }
    }
}

impl<F> FetcherService<F> {
    pub fn new(fetcher: F) -> Self {
        Self {
            fetcher: Arc::new(fetcher),
        }
    }

    pub fn into_inner(self) -> Arc<F> {
        self.fetcher
    }
}

impl<F> tower::Service<SubgraphRequest> for FetcherService<F>
where
    F: Fetcher + 'static,
{
    type Response = RouterResponse;
    type Error = FetchError;
    type Future = BoxFuture<'static, Result<Self::Response, FetchError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), FetchError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let fetcher = self.fetcher.clone();
        Box::pin(async move { fetcher.stream(&request).await })
    }
}

#[async_trait::async_trait]
impl<F> Fetcher for FetcherService<F>
where
    F: Fetcher + 'static,
{
    async fn stream(&self, request: &SubgraphRequest) -> Result<RouterResponse, FetchError> {
        self.fetcher.stream(request).await
    }
}
