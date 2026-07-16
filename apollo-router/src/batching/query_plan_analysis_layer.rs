use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;

use crate::batching::BatchQuery;
use crate::graphql;
use crate::services::execution;

/// Analyze the query plan for a batch query.
///
/// This informs the [`BatchQuery`] about the query plan fetch node hashes.
///
/// It also reject the request if it contains any subscription or defer nodes, which is not
/// supported in batch requests.
#[derive(Clone)]
pub(crate) struct BatchQueryPlanAnalysisLayer {
    _private: (),
}

impl BatchQueryPlanAnalysisLayer {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for BatchQueryPlanAnalysisLayer {
    type Service = BatchQueryPlanAnalysisService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BatchQueryPlanAnalysisService { inner }
    }
}

#[derive(Clone)]
pub(crate) struct BatchQueryPlanAnalysisService<S> {
    inner: S,
}

impl<S> tower::Service<execution::Request> for BatchQueryPlanAnalysisService<S>
where
    S: tower::Service<execution::Request, Response = execution::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = execution::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: execution::Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            let plan = &req.query_plan;
            let variables = &req.supergraph_request.body().variables;
            let is_deferred = plan.is_deferred(variables);
            let is_subscription = plan.is_subscription();

            let Some(batching) = req
                .context
                .extensions()
                .with_lock(|lock| lock.get::<crate::configuration::Batching>().cloned())
            else {
                return inner.call(req).await.map_err(Into::into);
            };

            if batching.enabled && (is_deferred || is_subscription) {
                let code = if is_deferred {
                    "BATCHING_DEFER_UNSUPPORTED"
                } else {
                    "BATCHING_SUBSCRIPTION_UNSUPPORTED"
                };
                let mut response = execution::Response::new_from_graphql_response(
                        graphql::Response::builder()
                        .error(crate::error::Error::builder()
                            .message("Deferred responses and subscriptions aren't supported in batches")
                            .extension_code(code)
                            .build())
                            .build(),
                        req.context,
                    );
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                return Ok(response);
            }

            // Now perform query batch analysis
            let batch_query_opt = req
                .context
                .extensions()
                .with_lock(|lock| lock.get::<BatchQuery>().cloned());
            if let Some(batch_query) = batch_query_opt {
                let query_hashes = plan.query_hashes(batching, variables)?;
                batch_query.set_query_hashes(query_hashes).await?;
                tracing::debug!("batch registered: {}", batch_query);
            }

            inner.call(req).await.map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;
    use std::sync::Arc;

    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::BatchQueryPlanAnalysisLayer;
    use crate::Configuration;
    use crate::Context;
    use crate::batching::Batch;
    use crate::batching::BatchQuery;
    use crate::compute_job::ComputeJobType;
    use crate::graphql;
    use crate::query_planner::QueryPlan;
    use crate::query_planner::QueryPlannerService;
    use crate::services::QueryPlannerContent;
    use crate::services::QueryPlannerRequest;
    use crate::services::execution;
    use crate::spec::Query;
    use crate::spec::Schema;

    async fn plan_query(
        query: &str,
        schema: Arc<Schema>,
        configuration: Arc<Configuration>,
    ) -> Arc<QueryPlan> {
        let document = Query::parse_document(query, None, &schema, &configuration).unwrap();

        let QueryPlannerContent::Plan {
            plan: query_plan, ..
        } = QueryPlannerService::for_test(schema, configuration)
            .unwrap()
            .oneshot(
                QueryPlannerRequest::builder()
                    .query(query)
                    .document(document)
                    .metadata(crate::plugins::authorization::CacheKeyMetadata::default())
                    .plan_options(crate::services::PlanOptions::default())
                    .compute_job_type(ComputeJobType::QueryPlanning)
                    .build(),
            )
            .await
            .unwrap()
            .content
            .unwrap()
        else {
            panic!("unexpected query planner output");
        };

        query_plan
    }

    #[tokio::test]
    async fn reject_defer_query_plans() {
        let (mock, handle) = tower_test::mock::pair::<execution::Request, execution::Response>();

        let mut service = ServiceBuilder::new()
            .layer(BatchQueryPlanAnalysisLayer::new())
            .service(mock);

        // Setup is 70% of the work!

        let configuration = Arc::new(
            Configuration::from_str(
                r#"
            batching:
                enabled: true
                mode: batch_http_link
                subgraph:
                    all:
                        enabled: true
        "#,
            )
            .unwrap(),
        );

        let schema = Arc::new(
            Schema::parse(
                include_str!("../../tests/fixtures/batching/schema.graphql"),
                &configuration,
            )
            .unwrap(),
        );

        let query = r#"
          {
            entryA(count: 1) { index }
            ... @defer {
              entryB(count: 1) { index }
            }
          }
        "#;

        let query_plan = plan_query(query, schema, configuration.clone()).await;

        let batch = Arc::new(Batch::spawn_handler(1));
        let batch_query = Batch::query_for_index(batch, 0).unwrap();

        let context = Context::new();
        context.extensions().with_lock(|lock| {
            lock.insert(configuration.batching.clone());
            lock.insert(batch_query);
        });

        let request = execution::Request::builder()
            .supergraph_request(http::Request::new(
                graphql::Request::fake_builder().query(query).build(),
            ))
            .context(context)
            .query_plan(query_plan)
            .build();

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        assert!(response.contains_error_code("BATCHING_DEFER_UNSUPPORTED"));

        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn analyze_query_plan_nodes() {
        let (mock, mut handle) =
            tower_test::mock::pair::<execution::Request, execution::Response>();
        let driver = tokio::task::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();

            request.context.extensions().with_lock(|lock| {
                let batch_query = lock.get::<BatchQuery>().unwrap();
                assert_eq!(
                    batch_query
                        .remaining
                        .load(std::sync::atomic::Ordering::Relaxed),
                    2,
                    "both subgraph requests should be identified as batch-able"
                );
            });

            // Succeed the request
            responder.send_response(
                execution::Response::builder()
                    .data(serde_json_bytes::json!({
                        "entryA": [{ "index": 20 }],
                        "entryB": [{ "index": 30 }],
                    }))
                    .context(request.context)
                    .build()
                    .unwrap(),
            );
        });

        let mut service = ServiceBuilder::new()
            .layer(BatchQueryPlanAnalysisLayer::new())
            .service(mock);

        // Setup is 70% of the work!

        let configuration = Arc::new(
            Configuration::from_str(
                r#"
            batching:
                enabled: true
                mode: batch_http_link
                subgraph:
                    all:
                        enabled: true
        "#,
            )
            .unwrap(),
        );

        let schema = Arc::new(
            Schema::parse(
                include_str!("../../tests/fixtures/batching/schema.graphql"),
                &configuration,
            )
            .unwrap(),
        );

        let query = r#"
          {
            entryA(count: 1) { index }
            entryB(count: 1) { index }
          }
        "#;

        let query_plan = plan_query(query, schema, configuration.clone()).await;

        let batch = Arc::new(Batch::spawn_handler(1));
        let batch_query = Batch::query_for_index(batch, 0).unwrap();

        let context = Context::new();
        context.extensions().with_lock(|lock| {
            lock.insert(configuration.batching.clone());
            lock.insert(batch_query);
        });

        let request = execution::Request::builder()
            .supergraph_request(http::Request::new(
                graphql::Request::fake_builder().query(query).build(),
            ))
            .context(context)
            .query_plan(query_plan)
            .build();

        let _response = service.ready().await.unwrap().call(request).await.unwrap();

        crate::plugin::test::await_mock_driver(driver).await;
    }
}
