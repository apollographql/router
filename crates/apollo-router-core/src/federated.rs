use crate::prelude::graphql::*;
use derivative::Derivative;
use futures::prelude::*;
use std::pin::Pin;
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

#[allow(unused_mut, clippy::let_and_return)]
impl Fetcher for FederatedGraph {
    fn stream(&self, request: Request) -> Pin<Box<dyn Future<Output = ResponseStream> + Send>> {
        let federated_query_span = tracing::info_span!("federated");
        tracing::trace!("Request received:\n{:#?}", request);

        if let Some(introspection_response) =
            federated_query_span.in_scope(|| self.naive_introspection.get(&request.query))
        {
            return Box::pin(async { stream::iter(vec![introspection_response]).boxed() });
        }

        let query_planner = Arc::clone(&self.query_planner);
        let service_registry = Arc::clone(&self.service_registry);
        let schema = Arc::clone(&self.schema);
        let request = Arc::new(request);

        Box::pin(
            async move {
                let query_plan = {
                    match query_planner
                        .get(
                            request.query.as_str().to_owned(),
                            request.operation_name.to_owned(),
                            Default::default(),
                        )
                        .instrument(tracing::info_span!("plan"))
                        .await
                    {
                        Ok(query_plan) => query_plan,
                        Err(err) => {
                            return stream::iter(vec![FetchError::from(err).to_response(true)])
                                .boxed()
                        }
                    }
                };

                if let Some(plan) = query_plan.node() {
                    tracing::debug!("query plan\n{:#?}", plan);

                    let early_errors_response = tracing::info_span!("validation").in_scope(|| {
                        let mut early_errors = Vec::new();
                        for err in
                            plan.validate_services_against_plan(Arc::clone(&service_registry))
                        {
                            early_errors.push(err.to_graphql_error(None));
                        }

                        for err in plan.validate_request_variables_against_plan(&request) {
                            early_errors.push(err.to_graphql_error(None));
                        }

                        // If we have any errors so far then let's abort the query
                        // Planning/validation/variables are candidates to abort.
                        if !early_errors.is_empty() {
                            tracing::error!(errors = format!("{:?}", early_errors).as_str());
                            let response = Response::builder().errors(early_errors).build();
                            Some(stream::once(async move { response }).boxed())
                        } else {
                            None
                        }
                    });

                    if let Some(response) = early_errors_response {
                        return response;
                    }
                } else {
                    return stream::empty().boxed();
                }

                let query_execution_span = tracing::info_span!("execution");
                stream::once(
                    async move {
                        let response = query_plan
                            .node()
                            .expect("we already ensured that the plan is some; qed")
                            .execute(
                                request.clone(),
                                Arc::clone(&service_registry),
                                Arc::clone(&schema),
                            )
                            .instrument(query_execution_span)
                            .await;

                        // TODO
                        /*
                        #[cfg(feature = "post-processing")]
                        tracing::debug_span!("format_response")
                            .in_scope(|| request.query.format_response(&mut response));
                        */

                        response
                    }
                    .with_current_subscriber(),
                )
                .boxed()
            }
            .instrument(federated_query_span),
        )
    }
}
