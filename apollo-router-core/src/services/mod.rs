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

#[derive(Debug)]
pub struct QueryPlannerService<Q> {
    query_planner: Arc<Q>,
}

impl<Q> Clone for QueryPlannerService<Q> {
    fn clone(&self) -> Self {
        Self {
            query_planner: self.query_planner.clone(),
        }
    }
}

impl<Q> QueryPlannerService<Q> {
    pub fn new(query_planner: Arc<Q>) -> Self {
        Self { query_planner }
    }

    pub fn into_inner(self) -> Arc<Q> {
        self.query_planner
    }

    // TODO this should normally be called get() but it's conflicting with the trait's
    // implementation
    pub fn get_ref(&self) -> &Q {
        &self.query_planner
    }
}

impl<Q> tower::Service<QueryPlannerRequest> for QueryPlannerService<Q>
where
    Q: QueryPlanner + 'static,
{
    type Response = PlannedRequest;
    type Error = QueryPlannerError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(
        &mut self,
        QueryPlannerRequest { options, context }: QueryPlannerRequest,
    ) -> Self::Future {
        let query_planner = self.query_planner.clone();
        Box::pin(async move {
            let query_plan = query_planner
                .get(
                    context.request.body().query.as_str().to_owned(),
                    context.request.body().operation_name.to_owned(),
                    options,
                )
                .await?;
            Ok(PlannedRequest {
                query_plan,
                context,
            })
        })
    }
}

#[async_trait::async_trait]
impl<Q> QueryPlanner for QueryPlannerService<Q>
where
    Q: QueryPlanner + 'static,
{
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        self.query_planner.get(query, operation, options).await
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

/// TODO confusing name since this is a Response
pub struct PlannedRequest {
    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

pub struct SubgraphRequest {
    pub http_request: http::Request<Request>,

    pub context: Context,
}

pub struct QueryPlannerRequest {
    pub options: QueryPlanOptions,

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

// This code is inspired from:
// https://users.rust-lang.org/t/stalemated-between-impl-and-dyn-vicious-cycle/65546/3
//
// I only added `clone_box()` which is itself inspired from:
// https://docs.rs/tower/latest/src/tower/util/boxed_clone.rs.html#111-130
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
