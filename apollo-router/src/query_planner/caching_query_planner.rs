// This entire file is license key functionality

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::task;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use query_planner::QueryPlannerPlugin;
use router_bridge::planner::Planner;
use router_bridge::planner::UsageReporting;
use sha2::Digest;
use sha2::Sha256;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use crate::cache::DeduplicatingCache;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::QueryPlanResult;
use crate::services::query_planner;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn QueryPlannerPlugin>>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Clone)]
pub(crate) struct CachingQueryPlanner<T: Clone> {
    cache: Arc<
        DeduplicatingCache<CachingQueryKey, Result<QueryPlannerContent, Arc<QueryPlannerError>>>,
    >,
    delegate: T,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
    configuration: Arc<Configuration>,
}

impl<T: Clone + 'static> CachingQueryPlanner<T>
where
    T: tower::Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = QueryPlannerError,
        > + Send,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub(crate) async fn new(
        delegate: T,
        schema: Arc<Schema>,
        configuration: Arc<Configuration>,
        plugins: Plugins,
    ) -> CachingQueryPlanner<T> {
        let cache = Arc::new(
            DeduplicatingCache::from_configuration(
                &configuration.supergraph.query_planning.experimental_cache,
                "query planner",
            )
            .await,
        );
        Self {
            cache,
            delegate,
            schema,
            configuration,
            plugins: Arc::new(plugins),
        }
    }

    pub(crate) async fn cache_keys(&self, count: usize) -> Vec<(String, Option<String>)> {
        let keys = self.cache.in_memory_keys().await;
        keys.into_iter()
            .take(count)
            .map(|key| (key.query, key.operation))
            .collect()
    }

    pub(crate) async fn warm_up(&mut self, cache_keys: Vec<(String, Option<String>)>) {
        let schema_id = self.schema.schema_id.clone();

        let mut service = ServiceBuilder::new().service(
            self.plugins
                .iter()
                .rev()
                .fold(self.delegate.clone().boxed(), |acc, (_, e)| {
                    e.query_planner_service(acc)
                }),
        );

        let mut count = 0usize;
        for (query, operation) in cache_keys {
            let caching_key = CachingQueryKey {
                schema_id: schema_id.clone(),
                query: query.clone(),
                operation: operation.to_owned(),
            };
            let context = Context::new();

            let entry = self.cache.get(&caching_key).await;
            if entry.is_first() {
                let (compiler, file_id) =
                    Query::make_compiler(&query, &self.schema, &self.configuration);

                let request = QueryPlannerRequest {
                    query,
                    operation_name: operation,
                    context: context.clone(),
                    compiler,
                    file_id,
                };

                let res = match service.ready().await {
                    Ok(service) => service.call(request).await,
                    Err(_) => break,
                };

                match res {
                    Ok(QueryPlannerResponse { content, .. }) => {
                        if let Some(content) = &content {
                            count += 1;
                            entry.insert(Ok(content.clone())).await;
                        }
                    }
                    Err(error) => {
                        count += 1;
                        let e = Arc::new(error);
                        entry.insert(Err(e.clone())).await;
                    }
                }
            }
        }

        tracing::debug!("warmed up the query planner cache with {count} queries");
    }
}

impl CachingQueryPlanner<BridgeQueryPlanner> {
    pub(crate) fn planner(&self) -> Arc<Planner<QueryPlanResult>> {
        self.delegate.planner()
    }
}

impl<T: Clone + Send + 'static> tower::Service<query_planner::CachingRequest>
    for CachingQueryPlanner<T>
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

    fn call(&mut self, request: query_planner::CachingRequest) -> Self::Future {
        let mut qp = self.clone();
        let schema_id = self.schema.schema_id.clone();
        let schema = self.schema.clone();
        let configuration = self.configuration.clone();
        Box::pin(async move {
            let caching_key = CachingQueryKey {
                schema_id,
                query: request.query.clone(),
                operation: request.operation_name.to_owned(),
            };

            let context = request.context.clone();
            let entry = qp.cache.get(&caching_key).await;
            if entry.is_first() {
                let query_planner::CachingRequest {
                    query,
                    operation_name,
                    context,
                } = request;

                let (compiler, file_id) = Query::make_compiler(&query, &schema, &configuration);

                let request = QueryPlannerRequest::builder()
                    .query(query)
                    .and_operation_name(operation_name)
                    .context(context)
                    .compiler(compiler)
                    .file_id(file_id)
                    .build();

                // some clients might timeout and cancel the request before query planning is finished,
                // so we execute it in a task that can continue even after the request was canceled and
                // the join handle was dropped. That way, the next similar query will use the cache instead
                // of restarting the query planner until another timeout
                tokio::task::spawn(
                    async move {
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
                                    context
                                        .private_entries
                                        .lock()
                                        .insert(plan.usage_reporting.clone());
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
                    }
                    .in_current_span(),
                )
                .await
                .map_err(|e| {
                    CacheResolverError::RetrievalError(Arc::new(QueryPlannerError::JoinError(
                        e.to_string(),
                    )))
                })?
            } else {
                let res = entry
                    .get()
                    .await
                    .map_err(|_| QueryPlannerError::UnhandledPlannerResult)?;

                match res {
                    Ok(content) => {
                        if let QueryPlannerContent::Plan { plan, .. } = &content {
                            context
                                .private_entries
                                .lock()
                                .insert(plan.usage_reporting.clone());
                        }

                        Ok(QueryPlannerResponse::builder()
                            .content(content)
                            .context(context)
                            .build())
                    }
                    Err(error) => {
                        match error.deref() {
                            QueryPlannerError::PlanningErrors(pe) => {
                                request
                                    .context
                                    .private_entries
                                    .lock()
                                    .insert(pe.usage_reporting.clone());
                            }
                            QueryPlannerError::SpecError(e) => {
                                request
                                    .context
                                    .private_entries
                                    .lock()
                                    .insert(UsageReporting {
                                        stats_report_key: e.get_error_key().to_string(),
                                        referenced_fields_by_type: HashMap::new(),
                                    });
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
        let mut hasher = Sha256::new();
        hasher.update(&self.query);
        let query = hex::encode(hasher.finalize());

        let mut hasher = Sha256::new();
        hasher.update(self.operation.as_deref().unwrap_or("-"));
        let operation = hex::encode(hasher.finalize());

        write!(
            f,
            "plan.{}.{}.{}",
            self.schema_id.as_deref().unwrap_or("-"),
            query,
            operation
        )
    }
}

#[cfg(test)]
mod tests {
    use mockall::mock;
    use mockall::predicate::*;
    use router_bridge::planner::UsageReporting;
    use test_log::test;
    use tower::Service;

    use super::*;
    use crate::error::PlanErrors;
    use crate::query_planner::QueryPlan;
    use crate::spec::Query;

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

        let configuration = Arc::new(crate::Configuration::default());
        let schema = Arc::new(
            Schema::parse(
                include_str!("testdata/schema.graphql"),
                &configuration,
                None,
            )
            .unwrap(),
        );

        let mut planner =
            CachingQueryPlanner::new(delegate, schema, configuration, IndexMap::new()).await;

        for _ in 0..5 {
            assert!(planner
                .call(query_planner::CachingRequest::new(
                    "{ me { id } }".into(),
                    Some("".into()),
                    Context::new()
                ))
                .await
                .is_err());
        }

        assert!(planner
            .call(query_planner::CachingRequest::new(
                "{ me { id name } }".into(),
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

        let configuration = Arc::new(crate::Configuration::default());
        let schema = Arc::new(
            Schema::parse(
                include_str!("testdata/schema.graphql"),
                &configuration,
                None,
            )
            .unwrap(),
        );
        let mut planner =
            CachingQueryPlanner::new(delegate, schema, configuration, IndexMap::new()).await;

        for _ in 0..5 {
            assert!(planner
                .call(query_planner::CachingRequest::new(
                    "{ me { id } }".into(),
                    Some("".into()),
                    Context::new()
                ))
                .await
                .unwrap()
                .context
                .private_entries
                .lock()
                .contains_key::<UsageReporting>());
        }
    }
}
