use std::future;
use std::sync::Arc;
use std::time::Duration;

use tower::retry::budget::Budget as _;
use tower::retry::budget::TpsBudget;
use tower::retry::Policy;

use crate::plugins::telemetry::config_new::attributes::SubgraphRequestResendCountKey;
use crate::query_planner::OperationKind;
use crate::services::subgraph;

#[derive(Clone, Default)]
pub(crate) struct RetryPolicy {
    budget: Arc<TpsBudget>,
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
            budget: Arc::new(TpsBudget::new(
                duration.unwrap_or_else(|| Duration::from_secs(10)),
                min_per_sec.unwrap_or(10),
                retry_percent.unwrap_or(0.2),
            )),
            retry_mutations: retry_mutations.unwrap_or(false),
        }
    }
}

impl<Res, E> Policy<subgraph::Request, Res, E> for RetryPolicy {
    type Future = future::Ready<()>;

    fn retry(
        &mut self,
        req: &mut subgraph::Request,
        result: &mut Result<Res, E>,
    ) -> Option<Self::Future> {
        let subgraph_name = req.subgraph_name.clone().unwrap_or_default();
        match result {
            Ok(_resp) => {
                // Treat all `Response`s as success,
                // so deposit budget and don't retry...
                self.budget.deposit();
                None
            }
            Err(_e) => {
                if req.operation_kind == OperationKind::Mutation && !self.retry_mutations {
                    return None;
                }

                let can_retry = self.budget.withdraw();
                if !can_retry {
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

                Some(future::ready(()))
            }
        }
    }

    fn clone_request(&mut self, req: &subgraph::Request) -> Option<subgraph::Request> {
        Some(req.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FetchError;
    use crate::graphql;
    use crate::http_ext;
    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_retry_with_error() {
        async {
            let mut retry = RetryPolicy::new(
                Some(Duration::from_secs(10)),
                Some(10),
                Some(0.2),
                Some(false),
            );

            let mut subgraph_req = subgraph::Request::fake_builder()
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
                    &mut subgraph_req,
                    &mut Err::<subgraph::Response, &Box<FetchError>>(&Box::new(
                        FetchError::SubrequestHttpError {
                            status_code: None,
                            service: String::from("my_subgraph_name_error"),
                            reason: String::from("cannot contact the subgraph"),
                        }
                    ))
                )
                .is_some());

            assert!(retry
                .retry(
                    &mut subgraph_req,
                    &mut Err::<subgraph::Response, &Box<FetchError>>(&Box::new(
                        FetchError::SubrequestHttpError {
                            status_code: None,
                            service: String::from("my_subgraph_name_error"),
                            reason: String::from("cannot contact the subgraph"),
                        }
                    ))
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
