use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::task;

use futures::future::BoxFuture;
use router_bridge::planner::UsageReporting;
use serde::Serialize;
use serde_json_bytes::value::Serializer;
use tower::ServiceExt;

use super::USAGE_REPORTING;
use crate::cache::DeduplicatingCache;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::services::QueryPlannerContent;
use crate::*;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner<T: Clone> {
    cache: Arc<
        DeduplicatingCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>,
    >,
    delegate: T,
    schema_id: Option<String>,
}

impl<T: Clone + 'static> CachingQueryPlanner<T>
where
    T: tower::Service<QueryPlannerRequest, Response = QueryPlannerResponse>,
{
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) async fn new(
        delegate: T,
        plan_cache_limit: usize,
        schema_id: Option<String>,
        redis_urls: Option<Vec<String>>,
    ) -> CachingQueryPlanner<T> {
        let cache = Arc::new(DeduplicatingCache::with_capacity(plan_cache_limit, redis_urls).await);
        Self {
            cache,
            delegate,
            schema_id,
        }
    }
}

impl<T: Clone + Send + 'static> tower::Service<QueryPlannerRequest> for CachingQueryPlanner<T>
where
    T: tower::Service<
        QueryPlannerRequest,
        Response = QueryPlannerResponse,
        Error = QueryPlannerError,
    >,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    type Response = QueryPlannerResponse;
    type Error = CacheResolverError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: QueryPlannerRequest) -> Self::Future {
        let mut qp = self.clone();
        let schema_id = self.schema_id.clone();
        Box::pin(async move {
            let caching_key = CachingQueryKey {
                schema_id,
                query: request.query.clone(),
                operation: request.operation_name.to_owned(),
            };

            let context = request.context.clone();
            let entry = qp.cache.get(&caching_key).await;
            if entry.is_first() {
                let res = qp.delegate.ready().await?.call(request).await;
                match res {
                    Ok(QueryPlannerResponse {
                        content,
                        context,
                        errors,
                    }) => {
                        if let Some(content) = &content {
                            entry.insert(Ok(content.clone())).await;
                        }

                        if let Some(QueryPlannerContent::Plan { plan, .. }) = &content {
                            match (&plan.usage_reporting).serialize(Serializer) {
                                Ok(v) => {
                                    context.insert_json_value(USAGE_REPORTING, v);
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "usage reporting was not serializable to context, {}",
                                        e
                                    );
                                }
                            }
                        }
                        Ok(QueryPlannerResponse {
                            content,
                            context,
                            errors,
                        })
                    }
                    Err(error) => {
                        let e = Arc::new(error);
                        entry.insert(Err(e.clone())).await;
                        Err(CacheResolverError::RetrievalError(e))
                    }
                }
            } else {
                let res = entry
                    .get()
                    .await
                    .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?;

                match res {
                    Ok(content) => {
                        if let QueryPlannerContent::Plan { plan, .. } = &content {
                            match (&plan.usage_reporting).serialize(Serializer) {
                                Ok(v) => {
                                    context.insert_json_value(USAGE_REPORTING, v);
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "usage reporting was not serializable to context, {}",
                                        e
                                    );
                                }
                            }
                        }

                        Ok(QueryPlannerResponse::builder()
                            .content(content)
                            .context(context)
                            .build())
                    }
                    Err(error) => {
                        match error.deref() {
                            QueryPlannerError::PlanningErrors(pe) => {
                                if let Err(inner_e) = request
                                    .context
                                    .insert(USAGE_REPORTING, pe.usage_reporting.clone())
                                {
                                    tracing::error!(
                                        "usage reporting was not serializable to context, {}",
                                        inner_e
                                    );
                                }
                            }
                            QueryPlannerError::SpecError(e) => {
                                if let Err(inner_e) = request.context.insert(
                                    USAGE_REPORTING,
                                    UsageReporting {
                                        stats_report_key: e.get_error_key().to_string(),
                                        referenced_fields_by_type: HashMap::new(),
                                    },
                                ) {
                                    tracing::error!(
                                        "usage reporting was not serializable to context, {}",
                                        inner_e
                                    );
                                }
                            }
                            _ => {}
                        }

                        Err(CacheResolverError::RetrievalError(error))
                    }
                }
            }
        })
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct CachingQueryKey {
    pub(crate) schema_id: Option<String>,
    pub(crate) query: String,
    pub(crate) operation: Option<String>,
}

impl std::fmt::Display for CachingQueryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "plan|{}|{}|{}",
            self.schema_id.as_deref().unwrap_or("-"),
            self.query,
            self.operation.as_deref().unwrap_or("-")
        )
    }
}

#[cfg(test)]
mod tests {
    use mockall::mock;
    use mockall::predicate::*;
    use query_planner::QueryPlan;
    use router_bridge::planner::UsageReporting;
    use test_log::test;
    use tower::Service;

    use super::*;
    use crate::error::PlanErrors;
    use crate::query_planner::QueryPlanOptions;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_call(
                &self,
                key: QueryPlannerRequest,
            ) -> Result<QueryPlannerResponse, QueryPlannerError>;
        }

        impl Clone for MyQueryPlanner {
            fn clone(&self) -> MockMyQueryPlanner;
        }
    }

    impl Service<QueryPlannerRequest> for MockMyQueryPlanner {
        type Response = QueryPlannerResponse;

        type Error = QueryPlannerError;

        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut task::Context<'_>,
        ) -> task::Poll<Result<(), Self::Error>> {
            task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
            let res = self.sync_call(req);
            Box::pin(async move { res })
        }
    }

    #[test(tokio::test)]
    async fn test_plan() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate.expect_clone().returning(|| {
            let mut planner = MockMyQueryPlanner::new();
            planner.expect_sync_call().times(0..2).returning(|_| {
                Err(QueryPlannerError::from(PlanErrors {
                    errors: Default::default(),
                    usage_reporting: UsageReporting {
                        stats_report_key: "this is a test key".to_string(),
                        referenced_fields_by_type: Default::default(),
                    },
                }))
            });
            planner
        });

        let mut planner = CachingQueryPlanner::new(delegate, 10, None, None).await;

        for _ in 0..5 {
            assert!(planner
                .call(QueryPlannerRequest::new(
                    "query1".into(),
                    Some("".into()),
                    Context::new()
                ))
                .await
                .is_err());
        }
        assert!(planner
            .call(QueryPlannerRequest::new(
                "query2".into(),
                Some("".into()),
                Context::new()
            ))
            .await
            .is_err());
    }

    macro_rules! test_query_plan {
        () => {
            include_str!("testdata/query_plan.json")
        };
    }

    #[test(tokio::test)]
    async fn test_usage_reporting() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate.expect_clone().returning(|| {
            let mut planner = MockMyQueryPlanner::new();
            planner.expect_sync_call().times(0..2).returning(|_| {
                let query_plan: QueryPlan = QueryPlan {
                    formatted_query_plan: Default::default(),
                    root: serde_json::from_str(test_query_plan!()).unwrap(),
                    options: QueryPlanOptions::default(),
                    usage_reporting: UsageReporting {
                        stats_report_key: "this is a test report key".to_string(),
                        referenced_fields_by_type: Default::default(),
                    },
                    query: Arc::new(Query::default()),
                };
                let qp_content = QueryPlannerContent::Plan {
                    plan: Arc::new(query_plan),
                };

                Ok(QueryPlannerResponse::builder()
                    .content(qp_content)
                    .context(Context::new())
                    .build())
            });
            planner
        });

        let mut planner = CachingQueryPlanner::new(delegate, 10, None, None).await;

        for _ in 0..5 {
            assert!(planner
                .call(QueryPlannerRequest::new(
                    "".into(),
                    Some("".into()),
                    Context::new()
                ))
                .await
                .unwrap()
                .context
                .get::<_, UsageReporting>(USAGE_REPORTING)
                .ok()
                .flatten()
                .is_some());
        }
    }
}
