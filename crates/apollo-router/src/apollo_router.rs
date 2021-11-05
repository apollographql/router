use apollo_router_core::{
    extensions::{ExecutionContext, Extensions},
    prelude::graphql::*,
};
use derivative::Derivative;
use futures::prelude::*;
use std::sync::Arc;
use tracing_futures::WithSubscriber;
use wasmtime::Val;

/// The default router of Apollo, suitable for most use cases.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ApolloRouter {
    #[derivative(Debug = "ignore")]
    naive_introspection: NaiveIntrospection,
    query_planner: Arc<dyn QueryPlanner>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    #[derivative(Debug = "ignore")]
    extensions: Extensions,
}

impl ApolloRouter {
    /// Create an [`ApolloRouter`] instance used to execute a GraphQL query.
    pub fn new(
        query_planner: Arc<dyn QueryPlanner>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
        extensions: Extensions,
    ) -> Self {
        Self {
            naive_introspection: NaiveIntrospection::from_schema(&schema),
            query_planner,
            service_registry,
            schema,
            extensions,
        }
    }
}

#[async_trait::async_trait]
impl Router<ApolloPreparedQuery> for ApolloRouter {
    #[tracing::instrument]
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

        // TODO query caching
        let query = Arc::new(Query::from(&request.query));

        let mut execution_context = self.extensions.context();
        tracing::info!("created execution context. it will live for the entire session");

        Ok(ApolloPreparedQuery {
            query_plan,
            service_registry: Arc::clone(&self.service_registry),
            schema: Arc::clone(&self.schema),
            query,
            execution_context,
        })
    }
}

// The default route used with [`ApolloRouter`], suitable for most use cases.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ApolloPreparedQuery {
    query_plan: Arc<QueryPlan>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
    // TODO
    #[allow(dead_code)]
    query: Arc<Query>,
    #[derivative(Debug = "ignore")]
    execution_context: ExecutionContext,
}

#[async_trait::async_trait]
impl PreparedQuery for ApolloPreparedQuery {
    #[tracing::instrument]
    async fn execute(mut self, request: Arc<Request>) -> ResponseStream {
        stream::once(
            async move {
                // get an instance for the "launch" hook
                let instance = self
                    .execution_context
                    .instantiate("launch".to_string())
                    .unwrap();
                tracing::info!("created instance of a wasm module");

                let hello = instance
                    .get_func(&mut self.execution_context.store, "hello")
                    .expect("`hello` was not an exported function");
                tracing::info!("got the hello function");

                /*let res = hello
                .typed::<(i32, u64), u64, _>(&execution_context.store)
                .unwrap();*/

                let world = "world";
                //FIXME: obviously invalid pointer
                let mut args = [Val::I32(world.as_ptr() as _), Val::I64(world.len() as i64)];
                let mut results = [Val::I64(1); 1];
                let result = hello
                    .call(
                        &mut self.execution_context.store,
                        &args[..],
                        &mut results[..],
                    )
                    .unwrap();
                println!("Answer: {:?}", results);

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
        .boxed()
    }
}
