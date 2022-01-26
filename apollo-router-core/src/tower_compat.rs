use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::task;

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
    type Response = RouterResponse;
    type Error = ();
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(
        &mut self,
        RouterRequest {
            http_request,
            context,
        }: RouterRequest,
    ) -> Self::Future {
        let router = self.router.clone();
        let context = context.with_request(Arc::new(http_request));
        Box::pin(async move {
            let response = match router.prepare_query(context.clone()).await {
                Ok(route) => route.execute(context.clone()).await,
                Err(response) => response,
            };

            Ok(RouterResponse {
                response: http::Response::new(response),
                context,
            })
        })
    }
}

#[async_trait::async_trait]
impl<R> Router for RouterService<R>
where
    R: Router + 'static,
{
    type PreparedQuery = R::PreparedQuery;

    async fn prepare_query(&self, context: Context) -> Result<Self::PreparedQuery, Response> {
        self.router.prepare_query(context).await
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

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), FetchError>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(
        &mut self,
        SubgraphRequest {
            http_request,
            context,
        }: SubgraphRequest,
    ) -> Self::Future {
        let fetcher = self.fetcher.clone();
        Box::pin(async move { fetcher.stream(http_request, context).await })
    }
}

#[async_trait::async_trait]
impl<F> Fetcher for FetcherService<F>
where
    F: Fetcher + 'static,
{
    async fn stream(
        &self,
        request: http::Request<Request>,
        context: Context,
    ) -> Result<RouterResponse, FetchError> {
        self.fetcher.stream(request, context).await
    }
}

// the parsed graphql Request, HTTP headers and contextual data for extensions
pub struct RouterRequest {
    pub http_request: http::Request<Request>,

    // Context for extension
    pub context: Context<()>,
}

impl From<http::Request<Request>> for RouterRequest {
    fn from(http_request: http::Request<Request>) -> Self {
        Self {
            http_request,
            context: Context::new(),
        }
    }
}

pub struct PlannedRequest {
    // hiding this one for now
    // pub query_plan: QueryPlan,

    // Cloned from RouterRequest
    pub context: Context,
}

pub struct SubgraphRequest {
    pub http_request: http::Request<Request>,

    // Cloned from PlannedRequest
    pub context: Context,
}

pub struct RouterResponse {
    pub response: http::Response<Response>,

    pub context: Context,
}

impl AsRef<Request> for http::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http::Request<Request>> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

pub(crate) trait DynCloneService<Request>: Send + Sync {
    type Response;
    type Error;

    fn ready<'a>(&'a mut self) -> BoxFuture<'a, Result<(), Self::Error>>;
    fn call(&mut self, req: Request) -> BoxFuture<'static, Result<Self::Response, Self::Error>>;
    fn clone_box(
        &self,
    ) -> Box<dyn DynCloneService<Request, Response = Self::Response, Error = Self::Error>>;
}

impl<T, R> DynCloneService<R> for T
where
    T: tower::Service<R> + Clone + Send + Sync + 'static,
    T::Future: Send + 'static,
{
    type Response = <T as tower::Service<R>>::Response;
    type Error = <T as tower::Service<R>>::Error;

    fn ready<'a>(&'a mut self) -> BoxFuture<'a, Result<(), Self::Error>> {
        Box::pin(futures::future::poll_fn(move |cx| self.poll_ready(cx)))
    }
    fn call(&mut self, req: R) -> BoxFuture<'static, Result<Self::Response, Self::Error>> {
        let fut = tower::Service::call(self, req);
        Box::pin(fut)
    }
    fn clone_box(
        &self,
    ) -> Box<dyn DynCloneService<R, Response = Self::Response, Error = Self::Error>> {
        Box::new(self.clone())
    }
}
