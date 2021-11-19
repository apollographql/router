use crate::prelude::graphql::*;
use derivative::Derivative;
use futures::prelude::*;
use std::sync::Arc;
use tracing::Instrument;
use tracing_futures::WithSubscriber;

// TODO move to apollo-router
/// A federated graph that can be queried.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct FederatedGraph {
    #[derivative(Debug = "ignore")]
    naive_introspection: NaiveIntrospection,
    query_planner: Arc<dyn QueryPlanner>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
}

// TODO move to apollo-router
impl Router<FederatedGraphRoute> for FederatedGraph {
    fn create_route(
        &self,
        request: Request,
    ) -> future::BoxFuture<'static, Result<FederatedGraphRoute, ResponseStream>> {
        if let Some(response) = self.naive_introspection.get(&request.query) {
            return future::ready(Err(response.into())).boxed();
        }

        let query_planner = Arc::clone(&self.query_planner);
        let service_registry = Arc::clone(&self.service_registry);
        let schema = Arc::clone(&self.schema);
        let request = Arc::new(request);

        async move {
            let query_plan = query_planner
                .get(
                    request.query.as_str().to_owned(),
                    request.operation_name.to_owned(),
                    Default::default(),
                )
                .await?;

            if let Some(plan) = query_plan.node() {
                tracing::debug!("query plan\n{:#?}", plan);
                plan.validate_request(&request, Arc::clone(&service_registry))?;
            } else {
                // TODO this should probably log something
                return Err(stream::empty().boxed());
            }

            // TODO query caching
            let query = Arc::new(Query::from(&request.query));

            Ok(FederatedGraphRoute {
                request,
                query_plan,
                service_registry: Arc::clone(&service_registry),
                schema: Arc::clone(&schema),
                query,
            })
        }
        .instrument(tracing::info_span!("route_creation"))
        .boxed()
    }
}

// TODO move to apollo-router
pub struct FederatedGraphRoute {
    request: Arc<Request>,
    query_plan: Arc<QueryPlan>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    query: Arc<Query>,
}

// TODO move to apollo-router
impl Route for FederatedGraphRoute {
    fn execute(self) -> ResponseStream {
        stream::once(
            async move {
                let mut response = self
                    .query_plan
                    .node()
                    .expect("we already ensured that the plan is some; qed")
                    .execute(
                        Arc::clone(&self.request),
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
        .boxed()
    }
}

impl FederatedGraph {
    /// Create a `FederatedGraph` instance used to execute a GraphQL query.
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
