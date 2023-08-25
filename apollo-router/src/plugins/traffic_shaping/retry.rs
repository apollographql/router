use std::future;
use std::sync::Arc;
use std::time::Duration;

use tower::retry::budget::Budget;
use tower::retry::Policy;

use crate::query_planner::OperationKind;
use crate::services::subgraph;

#[derive(Clone, Default)]
pub(crate) struct RetryPolicy {
    budget: Arc<Budget>,
    retry_mutations: bool,
    subgraph_name: String,
}

impl RetryPolicy {
    pub(crate) fn new(
        duration: Option<Duration>,
        min_per_sec: Option<u32>,
        retry_percent: Option<f32>,
        retry_mutations: Option<bool>,
        subgraph_name: String,
    ) -> Self {
        Self {
            budget: Arc::new(Budget::new(
                duration.unwrap_or_else(|| Duration::from_secs(10)),
                min_per_sec.unwrap_or(10),
                retry_percent.unwrap_or(0.2),
            )),
            retry_mutations: retry_mutations.unwrap_or(false),
            subgraph_name,
        }
    }
}

impl<Res, E> Policy<subgraph::Request, Res, E> for RetryPolicy {
    type Future = future::Ready<Self>;

    fn retry(&self, req: &subgraph::Request, result: Result<&Res, &E>) -> Option<Self::Future> {
        match result {
            Ok(_) => {
                // Treat all `Response`s as success,
                // so deposit budget and don't retry...
                self.budget.deposit();
                None
            }
            Err(_e) => {
                if req.operation_kind == OperationKind::Mutation && !self.retry_mutations {
                    return None;
                }

                let withdrew = self.budget.withdraw();
                if withdrew.is_err() {
                    tracing::info!(
                        monotonic_counter.apollo_router_http_request_retry_total = 1u64,
                        status = "aborted",
                        subgraph = %self.subgraph_name,
                    );

                    return None;
                }

                tracing::info!(
                    monotonic_counter.apollo_router_http_request_retry_total = 1u64,
                    subgraph = %self.subgraph_name,
                );

                Some(future::ready(self.clone()))
            }
        }
    }

    fn clone_request(&self, req: &subgraph::Request) -> Option<subgraph::Request> {
        Some(req.clone())
    }
}
