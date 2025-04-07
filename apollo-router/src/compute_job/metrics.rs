use std::time::Instant;

use crate::compute_job::ComputeJobType;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;

#[derive(strum_macros::Display)]
pub(super) enum Outcome {
    Executed,
    ExecutedError,
    RejectedQueueFull,
    Abandoned,
}

pub(super) struct JobWatcher {
    queue_start: Instant,
    compute_job_type: ComputeJobType,
    pub(super) outcome: Outcome,
}

impl JobWatcher {
    pub(super) fn new(compute_job_type: ComputeJobType) -> Self {
        Self {
            queue_start: Instant::now(),
            outcome: Outcome::Abandoned,
            compute_job_type,
        }
    }
}

impl Drop for JobWatcher {
    fn drop(&mut self) {
        let otel_status = match self.outcome {
            Outcome::Executed => OTEL_STATUS_CODE_OK,
            Outcome::Abandoned | Outcome::ExecutedError | Outcome::RejectedQueueFull => {
                OTEL_STATUS_CODE_ERROR
            }
        };

        let current_span = tracing::Span::current();
        current_span.record("job.outcome", self.outcome.to_string());
        current_span.record("otel.status_code", otel_status);

        let queue_duration = self.queue_start.elapsed();
        f64_histogram!(
            "apollo.router.compute_jobs.queue.jobs",
            "Information about the jobs",
            queue_duration.as_secs_f64(),
            "job.type" = self.compute_job_type.to_string(),
            "job.outcome" = self.outcome.to_string()
        );
    }
}

pub(super) struct QueueActiveMetric {
    compute_job_type: ComputeJobType,
}

impl QueueActiveMetric {
    // create metric (auto-increments and decrements)
    pub(super) fn register(compute_job_type: ComputeJobType) -> Self {
        let s = Self { compute_job_type };
        s.incr(1);
        s
    }

    fn incr(&self, value: i64) {
        i64_up_down_counter_with_unit!(
            "apollo.router.compute_jobs.active",
            "Number of computation jobs in progress",
            "s",
            value,
            job.type = self.compute_job_type.to_string()
        );
    }
}

impl Drop for QueueActiveMetric {
    fn drop(&mut self) {
        self.incr(-1);
    }
}
