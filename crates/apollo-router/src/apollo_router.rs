use apollo_router_core::prelude::graphql::*;
use derivative::Derivative;
use futures::prelude::*;
use std::sync::Arc;
use tracing::Instrument;
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
}

impl ApolloRouter {
    /// Create an [`ApolloRouter`] instance used to execute a GraphQL query.
    pub fn new(
        query_planner: Arc<dyn QueryPlanner>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
    ) -> Self {
        Self {
            naive_introspection: NaiveIntrospection::from_schema(&schema),
            query_planner,
            service_registry,
            schema,
        }
    }
}

impl Router<ApolloRoute> for ApolloRouter {
    fn create_route<'a>(
        &'a self,
        request: &'a Request,
    ) -> future::BoxFuture<'a, Result<ApolloRoute, ResponseStream>> {
        if let Some(response) = self.naive_introspection.get(&request.query) {
            return future::ready(Err(response.into())).boxed();
        }

        async move {
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

            // TODO query caching
            let query = Arc::new(Query::from(&request.query));

            Ok(ApolloRoute {
                query_plan,
                service_registry: Arc::clone(&self.service_registry),
                schema: Arc::clone(&self.schema),
                query,
            })
        }
        .instrument(tracing::info_span!("route_creation"))
        .boxed()
    }
}

// The default route used with [`ApolloRouter`], suitable for most use cases.
#[derive(Debug)]
pub struct ApolloRoute {
    query_plan: Arc<QueryPlan>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    // TODO
    #[allow(dead_code)]
    query: Arc<Query>,
}

impl Route for ApolloRoute {
    fn execute(self, request: Arc<Request>) -> future::BoxFuture<'static, ResponseStream> {
        future::ready(
            stream::once(
                async move {
                    // TODO
                    #[allow(unused_mut)]
                    let mut response = self
                        .query_plan
                        .node()
                        .expect("we already ensured that the plan is some; qed")
                        .execute(
                            request,
                            Arc::clone(&self.service_registry),
                            Arc::clone(&self.schema),
                        )
                        .await;

                    // TODO move query parsing to query creation
                    #[cfg(feature = "post-processing")]
                    tracing::debug_span!("format_response")
                        .in_scope(|| self.query.format_response(&mut response));

                    response
                }
                .with_current_subscriber(),
            )
            .boxed(),
        )
        .boxed()
    }
}
