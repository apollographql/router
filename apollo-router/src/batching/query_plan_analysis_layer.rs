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
    type Response = S::Response;
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

            let Some(batching) = req.context
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
            let batch_query_opt = req.context
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
