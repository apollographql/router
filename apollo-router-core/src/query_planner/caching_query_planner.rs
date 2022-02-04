use crate::prelude::graphql::*;
use crate::CacheResolver;
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task;
use tokio::sync::RwLock;

type PlanResult = Result<Arc<QueryPlan>, QueryPlannerError>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    cm: Arc<CachingMap<QueryKey, Arc<QueryPlan>>>,
    phantom: PhantomData<T>,
}

/// A resolver for cache misses
struct CachingQueryPlannerResolver<T: QueryPlanner> {
    delegate: T,
}

impl<T: QueryPlanner + 'static> CachingQueryPlanner<T> {
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub fn new(delegate: T, plan_cache_limit: usize) -> CachingQueryPlanner<T> {
        let resolver = CachingQueryPlannerResolver { delegate };
        let cm = Arc::new(CachingMap::new(Box::new(resolver), plan_cache_limit));
        Self {
            cm,
            phantom: PhantomData,
        }
    }

    pub async fn get_hot_keys(&self) -> Vec<QueryKey> {
        self.cm.get_hot_keys().await
    }
}

#[async_trait]
impl<T: QueryPlanner> CacheResolver<QueryKey, Arc<QueryPlan>> for CachingQueryPlannerResolver<T> {
    async fn retrieve(&self, key: QueryKey) -> Result<Arc<QueryPlan>, CacheResolverError> {
        self.delegate
            .get(key.0, key.1, key.2)
            .await
            .map_err(|err| err.into())
    }
}

#[async_trait]
impl<T: QueryPlanner> QueryPlanner for CachingQueryPlanner<T> {
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> PlanResult {
        let key = (query, operation, options);
        self.cm.get(key).await.map_err(|err| err.into())
    }
}

impl<T: QueryPlanner> tower::Service<RouterRequest> for CachingQueryPlanner<T>
where
    T: tower::Service<RouterRequest, Response = PlannedRequest, Error = tower::BoxError>,
{
    type Response = PlannedRequest;
    // TODO I don't think we can serialize this error back to the router response's payload
    type Error = tower::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RouterRequest) -> Self::Future {
        let body = request.http_request.body().clone();

        let cm = self.cm.clone();
        Box::pin(async move {
            // TODO: something with keyref and stuff
            if let Some(query) = body.query {
                let key = (
                    query,
                    body.operation_name.to_owned(),
                    QueryPlanOptions::default(),
                );
                cm.get(key)
                    .await
                    .map_err(|err| err.into())
                    .map(|query_plan| PlannedRequest {
                        query_plan,
                        context: Arc::new(RwLock::new(
                            request.context.with_request(Arc::new(request.http_request)),
                        )),
                    })
            } else {
                return Err(RequestError::NoQuery.into());
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::{mock, predicate::*};
    use router_bridge::plan::PlanningErrors;
    use std::sync::Arc;
    use test_log::test;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_get(
                &self,
                query: String,
                operation: Option<String>,
                options: QueryPlanOptions,
            ) -> PlanResult;
        }
    }

    #[async_trait]
    impl QueryPlanner for MockMyQueryPlanner {
        async fn get(
            &self,
            query: String,
            operation: Option<String>,
            options: QueryPlanOptions,
        ) -> PlanResult {
            self.sync_get(query, operation, options)
        }
    }

    #[test(tokio::test)]
    async fn test_plan() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate
            .expect_sync_get()
            .times(2)
            .return_const(Err(QueryPlannerError::PlanningErrors(Arc::new(
                PlanningErrors { errors: Vec::new() },
            ))));

        let planner = CachingQueryPlanner::new(delegate, 10);

        for _ in 0..5 {
            assert!(planner
                .get(
                    "query1".into(),
                    Some("".into()),
                    QueryPlanOptions::default()
                )
                .await
                .is_err());
        }
        assert!(planner
            .get(
                "query2".into(),
                Some("".into()),
                QueryPlanOptions::default()
            )
            .await
            .is_err());
    }
}
