use std::sync::Arc;
use std::{future, time::Duration};

use tower::retry::{budget::Budget, Policy};

#[derive(Clone, Default)]
pub(crate) struct RetryPolicy {
    budget: Arc<Budget>,
}

impl RetryPolicy {
    pub(crate) fn new(duration: Duration, min_per_sec: u32, retry_percent: f32) -> Self {
        Self {
            budget: Arc::new(Budget::new(duration, min_per_sec, retry_percent)),
        }
    }
}

impl<Req: Clone, Res, E> Policy<Req, Res, E> for RetryPolicy {
    type Future = future::Ready<Self>;

    fn retry(&self, req: &Req, result: Result<&Res, &E>) -> Option<Self::Future> {
        match result {
            Ok(_) => {
                // Treat all `Response`s as success,
                // so deposit budget and don't retry...
                self.budget.deposit();
                None
            }
            Err(e) => {
                let withdrew = self.budget.withdraw();
                if withdrew.is_err() {
                    return None;
                }

                Some(future::ready(self.clone()))
            }
        }
    }

    fn clone_request(&self, req: &Req) -> Option<Req> {
        Some(req.clone())
    }
}
