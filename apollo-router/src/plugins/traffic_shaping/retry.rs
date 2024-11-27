use std::future;
use std::sync::Arc;
use std::time::Duration;

use tower::retry::budget::Budget;
use tower::retry::Policy;

use crate::plugins::telemetry::config_new::attributes::SubgraphRequestResendCountKey;
use crate::query_planner::OperationKind;
use crate::services::subgraph;

#[derive(Clone, Default)]
pub(crate) struct RetryPolicy {
    budget: Arc<Budget>,
    retry_mutations: bool,
}

impl RetryPolicy {
    pub(crate) fn new(
        duration: Option<Duration>,
        min_per_sec: Option<u32>,
        retry_percent: Option<f32>,
        retry_mutations: Option<bool>,
    ) -> Self {
        Self {
            budget: Arc::new(Budget::new(
                duration.unwrap_or_else(|| Duration::from_secs(10)),
                min_per_sec.unwrap_or(10),
                retry_percent.unwrap_or(0.2),
            )),
            retry_mutations: retry_mutations.unwrap_or(false),
        }
    }
}

impl<E> Policy<subgraph::Request, subgraph::Response, E> for RetryPolicy {
    type Future = future::Ready<Self>;

    fn retry(
        &self,
        req: &subgraph::Request,
        result: Result<&subgraph::Response, &E>,
    ) -> Option<Self::Future> {
        let subgraph_name = req.subgraph_name.clone().unwrap_or_default();
        match result {
            Ok(resp) => {
                if resp.response.status() >= http::StatusCode::BAD_REQUEST {
                    if req.operation_kind == OperationKind::Mutation && !self.retry_mutations {
                        return None;
                    }

                    let withdrew = self.budget.withdraw();
                    if withdrew.is_err() {
                        u64_counter!(
                            "apollo_router_http_request_retry_total",
                            "Number of retries for an http request to a subgraph",
                            1u64,
                            status = "aborted",
                            subgraph = subgraph_name
                        );

                        return None;
                    }

                    let _ = req
                        .context
                        .upsert::<_, usize>(SubgraphRequestResendCountKey::new(&req.id), |val| {
                            val + 1
                        });

                    u64_counter!(
                        "apollo_router_http_request_retry_total",
                        "Number of retries for an http request to a subgraph",
                        1u64,
                        subgraph = subgraph_name
                    );

                    Some(future::ready(self.clone()))
                } else {
                    // Treat all `Response`s as success,
                    // so deposit budget and don't retry...
                    self.budget.deposit();
                    None
                }
            }
            Err(_e) => {
                if req.operation_kind == OperationKind::Mutation && !self.retry_mutations {
                    return None;
                }

                let withdrew = self.budget.withdraw();
                if withdrew.is_err() {
                    u64_counter!(
                        "apollo_router_http_request_retry_total",
                        "Number of retries for an http request to a subgraph",
                        1u64,
                        status = "aborted",
                        subgraph = subgraph_name
                    );

                    return None;
                }
                u64_counter!(
                    "apollo_router_http_request_retry_total",
                    "Number of retries for an http request to a subgraph",
                    1u64,
                    subgraph = subgraph_name
                );

                let _ = req
                    .context
                    .upsert::<_, usize>(SubgraphRequestResendCountKey::new(&req.id), |val| val + 1);

                Some(future::ready(self.clone()))
            }
        }
    }

    fn clone_request(&self, req: &subgraph::Request) -> Option<subgraph::Request> {
        Some(req.clone())
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use tower::BoxError;

    use super::*;
    use crate::error::FetchError;
    use crate::graphql;
    use crate::http_ext;
    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_retry_with_error() {
        async {
            let retry = RetryPolicy::new(
                Some(Duration::from_secs(10)),
                Some(10),
                Some(0.2),
                Some(false),
            );

            let subgraph_req = subgraph::Request::fake_builder()
                .subgraph_name("my_subgraph_name_error")
                .subgraph_request(
                    http_ext::Request::fake_builder()
                        .header("test", "my_value_set")
                        .body(
                            graphql::Request::fake_builder()
                                .query(String::from("query { test }"))
                                .build(),
                        )
                        .build()
                        .unwrap(),
                )
                .build();

            assert!(retry
                .retry(
                    &subgraph_req,
                    Err(&Box::new(FetchError::SubrequestHttpError {
                        status_code: None,
                        service: String::from("my_subgraph_name_error"),
                        reason: String::from("cannot contact the subgraph"),
                    }))
                )
                .is_some());

            assert!(retry
                .retry(
                    &subgraph_req,
                    Err(&Box::new(FetchError::SubrequestHttpError {
                        status_code: None,
                        service: String::from("my_subgraph_name_error"),
                        reason: String::from("cannot contact the subgraph"),
                    }))
                )
                .is_some());

            assert_counter!(
                "apollo_router_http_request_retry_total",
                2,
                "subgraph" = "my_subgraph_name_error"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_retry_with_http_status_code() {
        async {
            let retry = RetryPolicy::new(
                Some(Duration::from_secs(10)),
                Some(10),
                Some(0.2),
                Some(false),
            );

            let subgraph_req = subgraph::Request::fake_builder()
                .subgraph_name("my_subgraph_name_error")
                .subgraph_request(
                    http_ext::Request::fake_builder()
                        .header("test", "my_value_set")
                        .body(
                            graphql::Request::fake_builder()
                                .query(String::from("query { test }"))
                                .build(),
                        )
                        .build()
                        .unwrap(),
                )
                .build();

            assert!(retry
                .retry(
                    &subgraph_req,
                    Ok::<&subgraph::Response, &BoxError>(
                        &subgraph::Response::fake_builder()
                            .status_code(StatusCode::BAD_REQUEST)
                            .build()
                    )
                )
                .is_some());

            assert!(retry
                .retry(
                    &subgraph_req,
                    Ok::<&subgraph::Response, &BoxError>(
                        &subgraph::Response::fake_builder()
                            .status_code(StatusCode::BAD_REQUEST)
                            .build()
                    )
                )
                .is_some());

            assert_counter!(
                "apollo_router_http_request_retry_total",
                2,
                "subgraph" = "my_subgraph_name_error"
            );
        }
        .with_metrics()
        .await;
    }
}
