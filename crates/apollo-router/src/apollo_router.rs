use apollo_router_core::prelude::graphql::*;
use derivative::Derivative;
use futures::prelude::*;
use std::sync::Arc;
use tracing_futures::WithSubscriber;

/// The default router of Apollo, suitable for most use cases.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ApolloRouter {
    #[derivative(Debug = "ignore")]
    naive_introspection: NaiveIntrospection,
    query_planner: Arc<dyn QueryPlanner>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    query_cache: Arc<QueryCache>,
}

impl ApolloRouter {
    /// Create an [`ApolloRouter`] instance used to execute a GraphQL query.
    pub fn new(
        query_planner: Arc<dyn QueryPlanner>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
        query_cache_limit: usize,
    ) -> Self {
        Self {
            naive_introspection: NaiveIntrospection::from_schema(&schema),
            query_planner,
            service_registry,
            query_cache: Arc::new(QueryCache::new(query_cache_limit, Arc::clone(&schema))),
            schema,
        }
    }

    pub fn get_query_planner(&self) -> Arc<dyn QueryPlanner> {
        self.query_planner.clone()
    }
}

#[async_trait::async_trait]
impl Router<ApolloPreparedQuery> for ApolloRouter {
    #[tracing::instrument(level = "debug")]
    async fn prepare_query(
        &self,
        request: &Request,
    ) -> Result<ApolloPreparedQuery, ResponseStream> {
        if let Some(response) = self.naive_introspection.get(&request.query) {
            return Err(response.into());
        }

        let query_plan = self
            .query_planner
            .get(
                request.query.as_str().to_owned(),
                request.operation_name.to_owned(),
                Default::default(),
            )
            .await?;

        if let Some(plan) = query_plan.node() {
            tracing::debug!("query plan\n{:#?}", plan);
            plan.validate_request(request, Arc::clone(&self.service_registry))?;
        } else {
            // TODO this should probably log something
            return Err(stream::empty().boxed());
        }

        Ok(ApolloPreparedQuery {
            query_plan,
            service_registry: Arc::clone(&self.service_registry),
            schema: Arc::clone(&self.schema),
            query_cache: Arc::clone(&self.query_cache),
        })
    }
}

// The default route used with [`ApolloRouter`], suitable for most use cases.
#[derive(Debug)]
pub struct ApolloPreparedQuery {
    query_plan: Arc<QueryPlan>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    query_cache: Arc<QueryCache>,
}

#[async_trait::async_trait]
impl PreparedQuery for ApolloPreparedQuery {
    #[tracing::instrument(level = "debug")]
    async fn execute(self, request: Arc<Request>) -> ResponseStream {
        stream::once(
            async move {
                let response_task = self
                    .query_plan
                    .node()
                    .expect("we already ensured that the plan is some; qed")
                    .execute(
                        Arc::clone(&request),
                        Arc::clone(&self.service_registry),
                        Arc::clone(&self.schema),
                    );
                let query_task = self.query_cache.get_query(&request.query);

                let (mut response, query) = tokio::join!(response_task, query_task);

                if let Some(query) = query {
                    tracing::debug_span!("format_response").in_scope(|| {
                        query.format_response(&mut response, request.operation_name.as_deref())
                    });
                }

                response
            }
            .with_current_subscriber(),
        )
        .boxed()
    }
}
