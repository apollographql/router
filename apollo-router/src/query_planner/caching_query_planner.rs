use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::task;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use query_planner::QueryPlannerPlugin;
use rand::seq::SliceRandom;
use rand::thread_rng;
use router_bridge::planner::PlanOptions;
use router_bridge::planner::Planner;
use router_bridge::planner::UsageReporting;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;

use crate::cache::DeduplicatingCache;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::telemetry::utils::Timer;
use crate::query_planner::labeler::add_defer_labels;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::QueryPlanResult;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::query_planner;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
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
    enable_authorization_directives: bool,
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
        configuration: &Configuration,
        plugins: Plugins,
    ) -> Result<CachingQueryPlanner<T>, BoxError> {
        let cache = Arc::new(
            DeduplicatingCache::from_configuration(
                &configuration.supergraph.query_planning.cache,
                "query planner",
            )
            .await?,
        );

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(configuration, &schema).unwrap_or(false);
        Ok(Self {
            cache,
            delegate,
            schema,
            plugins: Arc::new(plugins),
            enable_authorization_directives,
        })
    }

    pub(crate) async fn cache_keys(&self, count: Option<usize>) -> Vec<WarmUpCachingQueryKey> {
        let keys = self.cache.in_memory_keys().await;
        let count = count.unwrap_or(keys.len() / 3);
        keys.into_iter()
            .take(count)
            .map(|key| WarmUpCachingQueryKey {
                query: key.query,
                operation: key.operation,
                metadata: key.metadata,
                plan_options: key.plan_options,
            })
            .collect()
    }

    pub(crate) async fn warm_up(
        &mut self,
        query_analysis: &QueryAnalysisLayer,
        persisted_query_layer: &PersistedQueryLayer,
        mut cache_keys: Vec<WarmUpCachingQueryKey>,
    ) {
        let _timer = Timer::new(|duration| {
            ::tracing::info!(
                histogram.apollo.router.query.planning.warmup.duration = duration.as_secs_f64()
            );
        });
        let schema_id = self.schema.schema_id.clone();

        let mut service = ServiceBuilder::new().service(
            self.plugins
                .iter()
                .rev()
                .fold(self.delegate.clone().boxed(), |acc, (_, e)| {
                    e.query_planner_service(acc)
                }),
        );

        cache_keys.shuffle(&mut thread_rng());

        let persisted_queries_operations = persisted_query_layer.all_operations();

        let capacity = cache_keys.len()
            + persisted_queries_operations
                .as_ref()
                .map(|ops| ops.len())
                .unwrap_or(0);
        tracing::info!(
            "warming up the query plan cache with {} queries, this might take a while",
            capacity
        );

        // persisted queries are added first because they should get a lower priority in the LRU cache,
        // since a lot of them may be there to support old clients
        let mut all_cache_keys = Vec::with_capacity(capacity);
        if let Some(queries) = persisted_queries_operations {
            for query in queries {
                all_cache_keys.push(WarmUpCachingQueryKey {
                    query,
                    operation: None,
                    metadata: CacheKeyMetadata::default(),
                    plan_options: PlanOptions::default(),
                });
            }
        }

        all_cache_keys.extend(cache_keys.into_iter());

        let mut count = 0usize;
        for WarmUpCachingQueryKey {
            mut query,
            operation,
            metadata,
            plan_options,
        } in all_cache_keys
        {
            let caching_key = CachingQueryKey {
                schema_id: schema_id.clone(),
                query: query.clone(),
                operation: operation.clone(),
                metadata,
                plan_options,
            };
            let context = Context::new();

            let entry = self.cache.get(&caching_key).await;
            if entry.is_first() {
                let doc = query_analysis.parse_document(&query);
                let err_res = Query::check_errors(&doc);
                if let Err(error) = err_res {
                    let e = Arc::new(QueryPlannerError::SpecError(error));
                    tokio::spawn(async move {
                        entry.insert(Err(e)).await;
                    });
                    continue;
                }

                let schema = &self.schema.api_schema().definitions;
                if let Ok(modified_query) = add_defer_labels(schema, &doc.ast) {
                    query = modified_query.to_string();
                }

                context.extensions().lock().insert::<ParsedDocument>(doc);

                context.extensions().lock().insert(caching_key.metadata);

                let request = QueryPlannerRequest {
                    query,
                    operation_name: operation,
                    context: context.clone(),
                };

                let res = match service.ready().await {
                    Ok(service) => service.call(request).await,
                    Err(_) => break,
                };

                match res {
                    Ok(QueryPlannerResponse { content, .. }) => {
                        if let Some(content) = content.clone() {
                            count += 1;
                            tokio::spawn(async move {
                                entry.insert(Ok(content.clone())).await;
                            });
                        }
                    }
                    Err(error) => {
                        count += 1;
                        let e = Arc::new(error);
                        tokio::spawn(async move {
                            entry.insert(Err(e)).await;
                        });
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
        let qp = self.clone();
        Box::pin(async move {
            let context = request.context.clone();
            qp.plan(request).await.map(|response| {
                if let Some(usage_reporting) =
                    context.extensions().lock().get::<Arc<UsageReporting>>()
                {
                    let _ = response.context.insert(
                        "apollo_operation_id",
                        stats_report_key_hash(usage_reporting.stats_report_key.as_str()),
                    );
                }
                response
            })
        })
    }
}

impl<T> CachingQueryPlanner<T>
where
    T: tower::Service<
            QueryPlannerRequest,
            Response = QueryPlannerResponse,
            Error = QueryPlannerError,
        > + Clone
        + Send
        + 'static,
    <T as tower::Service<QueryPlannerRequest>>::Future: Send,
{
    async fn plan(
        mut self,
        request: query_planner::CachingRequest,
    ) -> Result<<T as tower::Service<QueryPlannerRequest>>::Response, CacheResolverError> {
        let schema_id = self.schema.schema_id.clone();

        if self.enable_authorization_directives {
            AuthorizationPlugin::update_cache_key(&request.context);
        }

        let plan_options = PlanOptions {
            override_conditions: request
                .context
                .get(LABELS_TO_OVERRIDE_KEY)
                .unwrap_or_default()
                .unwrap_or_default(),
        };

        let caching_key = CachingQueryKey {
            schema_id,
            query: request.query.clone(),
            operation: request.operation_name.to_owned(),
            metadata: request
                .context
                .extensions()
                .lock()
                .get::<CacheKeyMetadata>()
                .cloned()
                .unwrap_or_default(),
            plan_options,
        };

        let context = request.context.clone();
        let entry = self.cache.get(&caching_key).await;
        if entry.is_first() {
            let query_planner::CachingRequest {
                mut query,
                operation_name,
                context,
            } = request;

            let doc = match context.extensions().lock().get::<ParsedDocument>() {
                None => {
                    return Err(CacheResolverError::RetrievalError(Arc::new(
                        QueryPlannerError::SpecError(SpecError::ParsingError(
                            "missing parsed document".to_string(),
                        )),
                    )))
                }
                Some(d) => d.clone(),
            };

            let schema = &self.schema.api_schema().definitions;
            if let Ok(modified_query) = add_defer_labels(schema, &doc.ast) {
                query = modified_query.to_string();
            }

            let request = QueryPlannerRequest::builder()
                .query(query)
                .and_operation_name(operation_name)
                .context(context)
                .build();

            // some clients might timeout and cancel the request before query planning is finished,
            // so we execute it in a task that can continue even after the request was canceled and
            // the join handle was dropped. That way, the next similar query will use the cache instead
            // of restarting the query planner until another timeout
            tokio::task::spawn(
                async move {
                    let err_res = Query::check_errors(&doc);

                    if let Err(error) = err_res {
                        request
                            .context
                            .extensions()
                            .lock()
                            .insert(Arc::new(UsageReporting {
                                stats_report_key: error.get_error_key().to_string(),
                                referenced_fields_by_type: HashMap::new(),
                            }));
                        let e = Arc::new(QueryPlannerError::SpecError(error));
                        let err = e.clone();
                        tokio::spawn(async move {
                            entry.insert(Err(err)).await;
                        });
                        return Err(CacheResolverError::RetrievalError(e));
                    }

                    let res = self.delegate.ready().await?.call(request).await;

                    match res {
                        Ok(QueryPlannerResponse {
                            content,
                            context,
                            errors,
                        }) => {
                            if let Some(content) = content.clone() {
                                tokio::spawn(async move {
                                    entry.insert(Ok(content)).await;
                                });
                            }

                            if let Some(QueryPlannerContent::Plan { plan, .. }) = &content {
                                context
                                    .extensions()
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
                            let err = e.clone();
                            tokio::spawn(async move {
                                entry.insert(Err(err)).await;
                            });
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
                            .extensions()
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
                                .extensions()
                                .lock()
                                .insert(pe.usage_reporting.clone());
                        }
                        QueryPlannerError::SpecError(e) => {
                            request
                                .context
                                .extensions()
                                .lock()
                                .insert(Arc::new(UsageReporting {
                                    stats_report_key: e.get_error_key().to_string(),
                                    referenced_fields_by_type: HashMap::new(),
                                }));
                        }
                        _ => {}
                    }

                    Err(CacheResolverError::RetrievalError(error))
                }
            }
        }
    }
}

fn stats_report_key_hash(stats_report_key: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(stats_report_key.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct CachingQueryKey {
    pub(crate) schema_id: Option<String>,
    pub(crate) query: String,
    pub(crate) operation: Option<String>,
    pub(crate) metadata: CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
}

const FEDERATION_VERSION: &str = std::env!("FEDERATION_VERSION");

impl std::fmt::Display for CachingQueryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut hasher = Sha256::new();
        hasher.update(&self.query);
        let query = hex::encode(hasher.finalize());

        let mut hasher = Sha256::new();
        hasher.update(self.operation.as_deref().unwrap_or("-"));
        let operation = hex::encode(hasher.finalize());

        let mut hasher = Sha256::new();
        hasher.update(&serde_json::to_vec(&self.metadata).expect("serialization should not fail"));
        hasher.update(
            &serde_json::to_vec(&self.plan_options).expect("serialization should not fail"),
        );
        let metadata = hex::encode(hasher.finalize());

        write!(
            f,
            "plan:{}:{}:{}:{}:{}",
            FEDERATION_VERSION,
            self.schema_id.as_deref().unwrap_or("-"),
            query,
            operation,
            metadata,
        )
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct WarmUpCachingQueryKey {
    pub(crate) query: String,
    pub(crate) operation: Option<String>,
    pub(crate) metadata: CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
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
    use crate::spec::Schema;
    use crate::Configuration;

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
            Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap(),
        );

        let mut planner =
            CachingQueryPlanner::new(delegate, schema, &configuration, IndexMap::new())
                .await
                .unwrap();

        let configuration = Configuration::default();

        let schema =
            Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap();

        let doc1 = Query::parse_document("query Me { me { username } }", &schema, &configuration);

        let context = Context::new();
        context.extensions().lock().insert::<ParsedDocument>(doc1);

        for _ in 0..5 {
            assert!(planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { username } }".to_string(),
                    Some("".into()),
                    context.clone()
                ))
                .await
                .is_err());
        }
        let doc2 = Query::parse_document(
            "query Me { me { name { first } } }",
            &schema,
            &configuration,
        );

        let context = Context::new();
        context.extensions().lock().insert::<ParsedDocument>(doc2);

        assert!(planner
            .call(query_planner::CachingRequest::new(
                "query Me { me { name { first } } }".to_string(),
                Some("".into()),
                context.clone()
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
                    }
                    .into(),
                    query: Arc::new(Query::empty()),
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

        let configuration = Configuration::default();

        let schema =
            Schema::parse(include_str!("testdata/schema.graphql"), &configuration).unwrap();

        let doc = Query::parse_document("query Me { me { username } }", &schema, &configuration);

        let mut planner =
            CachingQueryPlanner::new(delegate, Arc::new(schema), &configuration, IndexMap::new())
                .await
                .unwrap();

        let context = Context::new();
        context.extensions().lock().insert::<ParsedDocument>(doc);

        for _ in 0..5 {
            assert!(planner
                .call(query_planner::CachingRequest::new(
                    "query Me { me { username } }".to_string(),
                    Some("".into()),
                    context.clone(),
                ))
                .await
                .unwrap()
                .context
                .extensions()
                .lock()
                .contains_key::<Arc<UsageReporting>>());
        }
    }

    #[test]
    fn apollo_operation_id_hash() {
        assert_eq!(
            "d1554552698157b05c2a462827fb4367a4548ee5",
            stats_report_key_hash("# IgnitionMeQuery\nquery IgnitionMeQuery{me{id}}")
        );
    }
}
