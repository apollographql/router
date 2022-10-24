use std::sync::Arc;
use std::task;

use futures::future::BoxFuture;
// use router_bridge::planner::UsageReporting;
use serde::Serialize;
use serde_json_bytes::value::Serializer;

// use super::QueryKey;
use super::USAGE_REPORTING;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::services::QueryPlannerContent;
use crate::*;

use deduplicate::Deduplicate;
// use deduplicate::DeduplicateError;
use deduplicate::Retriever;

type Delegate = Arc<
    dyn Retriever<
        Key = QueryPlannerRequest,
        Value = Result<QueryPlannerResponse, QueryPlannerError>,
    >,
>;
/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner {
    cache: Arc<Deduplicate<QueryPlannerRequest, Result<QueryPlannerResponse, QueryPlannerError>>>,
}

impl CachingQueryPlanner {
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) async fn new(delegate: Delegate, plan_cache_limit: usize) -> CachingQueryPlanner {
        let cache = Arc::new(Deduplicate::with_capacity(delegate, plan_cache_limit).await);
        Self { cache }
    }
}

impl tower::Service<QueryPlannerRequest> for CachingQueryPlanner {
    type Response = QueryPlannerResponse;
    type Error = CacheResolverError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: QueryPlannerRequest) -> Self::Future {
        let qp = self.clone();
        Box::pin(async move {
            let _key = (request.query.clone(), request.operation_name.to_owned());
            let _context = request.context.clone();
            // TODO:This is hacker to help the testing of the crate go quickly
            // We need to do the proper handling of context USAGE REPORTING and error categorising
            // yet.
            let res = qp.cache.get(&request).await.map_err(|_| {
                CacheResolverError::RetrievalError(Arc::new(
                    QueryPlannerError::UnhandledPlannerResult,
                ))
            })?;
            match res {
                Ok(QueryPlannerResponse {
                    content,
                    context,
                    errors,
                }) => {
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
                    Err(CacheResolverError::RetrievalError(e))
                }
            }
            /*
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
            */
        })
    }
}

#[cfg(test)]
mod tests {
    use mockall::mock;
    use mockall::predicate::*;
    /*
    use query_planner::QueryPlan;
    use router_bridge::planner::PlanErrors;
    use router_bridge::planner::UsageReporting;
    use test_log::test;
    */
    use tower::Service;

    use super::*;
    // use crate::query_planner::QueryPlanOptions;

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

    /*
     * TODO: FIX THESE TESTS
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

        let mut planner = CachingQueryPlanner::new(delegate, 10).await;

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

        let mut planner = CachingQueryPlanner::new(delegate, 10).await;

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
    */
}
