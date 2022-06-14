use crate::CacheResolver;
use crate::*;
use async_trait::async_trait;
use futures::future::BoxFuture;
use router_bridge::planner::UsageReporting;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;
use std::task;

type PlanResult = Result<QueryPlannerContent, QueryPlannerError>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    cm: Arc<CachingMap<QueryKey, QueryPlannerContent>>,
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
impl<T: QueryPlanner> CacheResolver<QueryKey, QueryPlannerContent>
    for CachingQueryPlannerResolver<T>
{
    async fn retrieve(&self, key: QueryKey) -> Result<QueryPlannerContent, CacheResolverError> {
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

impl<T: QueryPlanner> tower::Service<QueryPlannerRequest> for CachingQueryPlanner<T>
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
        let body = request.originating_request.body();

        let key = (
            body.query
                .clone()
                .expect("presence of a query has been checked by the RouterService before; qed"),
            body.operation_name.to_owned(),
            request.query_plan_options,
        );
        let cm = self.cm.clone();
        Box::pin(async move {
            cm.get(key)
                .await
                .map(|query_plan| {
                    if let QueryPlannerContent::Plan { plan, .. } = &query_plan {
                        if let Err(e) = request
                            .context
                            .insert(USAGE_REPORTING, plan.usage_reporting.clone())
                        {
                            tracing::error!(
                                "usage reporting was not serializable to context, {}",
                                e
                            );
                        }
                    }
                    query_plan
                })
                .map_err(|e| {
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
    use super::*;
    use mockall::{mock, predicate::*};
    use router_bridge::planner::{PlanErrors, UsageReporting};
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
            .return_const(Err(QueryPlannerError::from(PlanErrors {
                errors: Default::default(),
                usage_reporting: UsageReporting {
                    stats_report_key: "this is a test key".to_string(),
                    referenced_fields_by_type: Default::default(),
                },
            })));

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
