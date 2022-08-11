use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::task;

use async_trait::async_trait;
use futures::future::BoxFuture;
use router_bridge::planner::UsageReporting;
use serde::Serialize;
use serde_json_bytes::value::Serializer;

use super::USAGE_REPORTING;
use crate::cache::DeduplicatingCache;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::services::QueryPlannerContent;
use crate::traits::CacheResolver;
use crate::traits::QueryKey;
use crate::traits::QueryPlanner;
use crate::*;

type PlanResult = Result<QueryPlannerContent, QueryPlannerError>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner<T: QueryPlanner + Clone> {
    cache: Arc<DeduplicatingCache<QueryKey, Result<QueryPlannerContent, QueryPlannerError>>>,
    delegate: Arc<T>,
}

/// A resolver for cache misses
struct CachingQueryPlannerResolver<T: QueryPlanner> {
    delegate: T,
}

impl<T: QueryPlanner + Clone + 'static> CachingQueryPlanner<T> {
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) async fn new(delegate: T, plan_cache_limit: usize) -> CachingQueryPlanner<T> {
        let cache = Arc::new(DeduplicatingCache::with_capacity(plan_cache_limit).await);
        Self {
            cache,
            delegate: Arc::new(delegate),
        }
    }
}

#[async_trait]
impl<T: QueryPlanner> CacheResolver<QueryKey, QueryPlannerContent>
    for CachingQueryPlannerResolver<T>
{
    async fn retrieve(&self, key: QueryKey) -> Result<QueryPlannerContent, CacheResolverError> {
        self.delegate.get(key).await.map_err(|err| err.into())
    }
}

#[async_trait]
impl<T: QueryPlanner + Clone> QueryPlanner for CachingQueryPlanner<T> {
    async fn get(&self, key: QueryKey) -> PlanResult {
        let entry = self.cache.get(&key).await;
        if entry.is_first() {
            let res = self.delegate.get(key).await;
            entry.insert(res.clone()).await;
            res
        } else {
            entry
                .get()
                .await
                .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?
        }
    }
}

impl<T: QueryPlanner + Clone + 'static> tower::Service<QueryPlannerRequest>
    for CachingQueryPlanner<T>
where
    T: tower::Service<
        QueryPlannerRequest,
        Response = QueryPlannerResponse,
        Error = tower::BoxError,
    >,
{
    type Response = QueryPlannerResponse;
    // TODO I don't think we can serialize this error back to the router response's payload
    type Error = tower::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: QueryPlannerRequest) -> Self::Future {
        let key = (
            request.query.clone(),
            request.operation_name.to_owned(),
            request.query_plan_options,
        );
        let qp = self.clone();
        Box::pin(async move {
            qp.get(key)
                .await
                .map(|query_planner_content| {
                    if let QueryPlannerContent::Plan { plan, .. } = &query_planner_content {
                        match (&plan.usage_reporting).serialize(Serializer) {
                            Ok(v) => {
                                request.context.insert_json_value(USAGE_REPORTING, v);
                            }
                            Err(e) => {
                                tracing::error!(
                                    "usage reporting was not serializable to context, {}",
                                    e
                                );
                            }
                        }
                    }
                    query_planner_content
                })
                .map_err(|e| {
                    let e = e.into();

                    let CacheResolverError::RetrievalError(re) = &e;
                    if let QueryPlannerError::PlanningErrors(pe) = re.deref() {
                        if let Err(inner_e) = request
                            .context
                            .insert(USAGE_REPORTING, pe.usage_reporting.clone())
                        {
                            tracing::error!(
                                "usage reporting was not serializable to context, {}",
                                inner_e
                            );
                        }
                    } else if let QueryPlannerError::SpecError(e) = re.deref() {
                        let error_key = match e {
                            SpecError::ParsingError(_) => "## GraphQLParseFailure\n",
                            _ => "## GraphQLValidationFailure\n",
                        };
                        if let Err(inner_e) = request.context.insert(
                            USAGE_REPORTING,
                            UsageReporting {
                                stats_report_key: error_key.to_string(),
                                referenced_fields_by_type: HashMap::new(),
                            },
                        ) {
                            tracing::error!(
                                "usage reporting was not serializable to context, {}",
                                inner_e
                            );
                        }
                    }

                    e.into()
                })
                .map(|query_plan| QueryPlannerResponse::new(query_plan, request.context))
        })
    }
}

#[cfg(test)]
mod tests {
    use mockall::mock;
    use mockall::predicate::*;
    use router_bridge::planner::PlanErrors;
    use router_bridge::planner::UsageReporting;
    use test_log::test;

    use super::*;
    use crate::query_planner::QueryPlanOptions;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_get(
                &self,
                key: QueryKey,
            ) -> PlanResult;
        }

        impl Clone for MyQueryPlanner {
            fn clone(&self) -> MockMyQueryPlanner;
        }
    }

    #[async_trait]
    impl QueryPlanner for MockMyQueryPlanner {
        async fn get(&self, key: QueryKey) -> PlanResult {
            self.sync_get(key)
        }
    }

    #[test(tokio::test)]
    async fn test_plan() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate
            .expect_sync_get()
            .times(2)
            .return_const(Err(QueryPlannerError::from(PlanErrors {
                errors: Default::default(),
                usage_reporting: UsageReporting {
                    stats_report_key: "this is a test key".to_string(),
                    referenced_fields_by_type: Default::default(),
                },
            })));

        let planner = CachingQueryPlanner::new(delegate, 10).await;

        for _ in 0..5 {
            assert!(planner
                .get((
                    "query1".into(),
                    Some("".into()),
                    QueryPlanOptions::default()
                ))
                .await
                .is_err());
        }
        assert!(planner
            .get((
                "query2".into(),
                Some("".into()),
                QueryPlanOptions::default()
            ))
            .await
            .is_err());
    }
}
