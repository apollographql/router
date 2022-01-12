use apollo_router_core::prelude::graphql::*;
use derivative::Derivative;
use futures::Future;
use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::Service;
use tracing::Instrument;

/// The default router of Apollo, suitable for most use cases.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ApolloRouter {
    #[derivative(Debug = "ignore")]
    naive_introspection: NaiveIntrospection,
    query_planner: Arc<CachingQueryPlanner<RouterBridgeQueryPlanner>>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    query_cache: Arc<QueryCache>,
}

impl ApolloRouter {
    /// Create an [`ApolloRouter`] instance used to execute a GraphQL query.
    pub async fn new(
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
        previous_router: Option<Arc<ApolloRouter>>,
    ) -> Self {
        let plan_cache_limit = std::env::var("ROUTER_PLAN_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        let query_cache_limit = std::env::var("ROUTER_QUERY_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        let query_planner = Arc::new(CachingQueryPlanner::new(
            RouterBridgeQueryPlanner::new(Arc::clone(&schema)),
            plan_cache_limit,
        ));

        // NaiveIntrospection instantiation can potentially block for some time
        let naive_introspection = {
            let schema = Arc::clone(&schema);
            tokio::task::spawn_blocking(move || NaiveIntrospection::from_schema(&schema))
                .await
                .expect("NaiveIntrospection instantiation panicked")
        };

        // Start warming up the cache
        //
        // We don't need to do this in background because the old server will keep running until
        // this one is ready.
        //
        // If we first warm up the cache in foreground, then switch to the new config, the next
        // queries will benefit from the warmed up cache. While if we switch and warm up in
        // background, the next queries might be blocked until the cache is primed, so there'll be
        // a perf hit.
        if let Some(previous_router) = previous_router {
            for (query, operation, options) in previous_router.query_planner.get_hot_keys().await {
                // We can ignore errors because some of the queries that were previously in the
                // cache might not work with the new schema
                let _ = query_planner.get(query, operation, options).await;
            }
        }

        Self {
            naive_introspection,
            query_planner,
            service_registry,
            query_cache: Arc::new(QueryCache::new(query_cache_limit)),
            schema,
        }
    }
}

#[async_trait::async_trait]
impl Router<ApolloPreparedQuery> for ApolloRouter {
    #[tracing::instrument(level = "debug", skip_all)]
    async fn prepare_query(&self, request: Arc<Request>) -> Result<ApolloPreparedQuery, Response> {
        if let Some(response) = self.naive_introspection.get(&request.query) {
            return Err(response);
        }

        let query = self
            .query_cache
            .get_query(&request.query)
            .instrument(tracing::info_span!("query_parsing"))
            .await;

        if let Some(query) = query.as_ref() {
            query.validate_variables(&request, &self.schema)?;
        }

        let query_plan = self
            .query_planner
            .get(
                request.query.as_str().to_owned(),
                request.operation_name.to_owned(),
                Default::default(),
            )
            .await?;

        tracing::debug!("query plan\n{:#?}", query_plan);
        query_plan.validate(Arc::clone(&self.service_registry))?;

        Ok(ApolloPreparedQuery {
            query_plan,
            service_registry: Arc::clone(&self.service_registry),
            schema: Arc::clone(&self.schema),
            query,
        })
    }
}

struct Planner {
    router: Arc<ApolloRouter>,
}
impl Service<Arc<Request>> for Planner {
    type Response = ApolloPreparedQuery;

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

// The default route used with [`ApolloRouter`], suitable for most use cases.
#[derive(Debug)]
pub struct ApolloPreparedQuery {
    query_plan: Arc<QueryPlan>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    query: Option<Arc<Query>>,
}

#[async_trait::async_trait]
impl PreparedQuery for ApolloPreparedQuery {
    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(self, request: Arc<Request>) -> Response {
        let mut response = self
            .query_plan
            .execute(&request, self.service_registry.as_ref(), &self.schema)
            .instrument(tracing::info_span!("execution"))
            .await;

        if let Some(query) = self.query {
            tracing::debug_span!("format_response").in_scope(|| {
                query.format_response(
                    &mut response,
                    request.operation_name.as_deref(),
                    &self.schema,
                )
            });
        }

        response
    }
}

pub struct ApolloRouterService<Router, PreparedQuery> {
    inner: Arc<Router>,
    _phantom: PhantomData<PreparedQuery>,
}

// we cannot derive Clone directly because that would require the Router and
// PreparedQuery types to be Clone
impl<Router, PreparedQuery> Clone for ApolloRouterService<Router, PreparedQuery> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: self._phantom.clone(),
        }
    }
}

//Sync is not derived for ApolloRouterService due to the PhantomData of PreparedQuery
// but that type does not actually affect the struct itself, it is only generated from
// inside Service::call
unsafe impl<Router, PreparedQuery> Sync for ApolloRouterService<Router, PreparedQuery> {}

impl<R, P> ApolloRouterService<R, P>
where
    R: Router<P> + 'static,
    P: PreparedQuery + Send + 'static,
{
    pub fn new(inner: Arc<R>) -> Self {
        ApolloRouterService {
            inner,
            _phantom: PhantomData,
        }
    }
}

impl<
        Router: apollo_router_core::Router<PreparedQuery> + 'static,
        PreparedQuery: apollo_router_core::PreparedQuery + Send + 'static,
    > Service<Request> for ApolloRouterService<Router, PreparedQuery>
{
    type Response = Response;

    type Error = ();

    type Future = ApolloRouterServiceFuture<
        Pin<Box<dyn Future<Output = Result<PreparedQuery, Response>> + Send>>,
        PreparedQuery,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let router = self.inner.clone();

        let req = Arc::new(req);
        let r2 = req.clone();
        let future = Box::pin(async move { router.prepare_query(req).await });
        ApolloRouterServiceFuture {
            state: State::Prepare { req: r2, future },
        }
    }
}

#[pin_project::pin_project]
pub struct ApolloRouterServiceFuture<F, P>
where
    F: Future<Output = Result<P, Response>> + Send,
    P: PreparedQuery + Send + 'static,
{
    #[pin]
    state: State<F, P>,
}

#[pin_project::pin_project(project = StateProj)]
enum State<F, P>
where
    F: Future<Output = Result<P, Response>> + Send,
    P: PreparedQuery + Send + 'static,
{
    Prepare {
        req: Arc<Request>,
        #[pin]
        future: F,
    },
    Execute {
        #[pin]
        exec: Pin<Box<dyn Future<Output = Response> + Send>>,
    },
}

impl<F, P> Future for ApolloRouterServiceFuture<F, P>
where
    F: Future<Output = Result<P, Response>> + Send,
    P: PreparedQuery + Send + 'static,
{
    type Output = Result<Response, ()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        loop {
            match this.state.as_mut().project() {
                StateProj::Prepare { req, future } => match future.poll(cx) {
                    Poll::Ready(res) => match res {
                        Err(response) => return Poll::Ready(Ok(response)),
                        Ok(prepared) => {
                            let exec = prepared.execute(req.clone());
                            this.state.set(State::Execute { exec })
                        }
                    },
                    Poll::Pending => {
                        return Poll::Pending;
                    }
                },
                StateProj::Execute { exec } => return exec.poll(cx).map(Ok),
            }
        }
    }
}
