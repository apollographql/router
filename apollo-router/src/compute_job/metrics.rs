use std::time::Duration;
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

        let full_duration = self.queue_start.elapsed();
        f64_histogram_with_unit!(
            "apollo.router.compute_jobs.duration",
            "Total job processing time",
            "s",
            full_duration.as_secs_f64(),
            "job.type" = self.compute_job_type.to_string(),
            "job.outcome" = self.outcome.to_string()
        );
    }
}

pub(super) struct ActiveComputeMetric {
    compute_job_type: ComputeJobType,
}

impl ActiveComputeMetric {
    // create metric (auto-increments and decrements)
    pub(super) fn register(compute_job_type: ComputeJobType) -> Self {
        let s = Self { compute_job_type };
        s.incr(1);
        s
    }

    fn incr(&self, value: i64) {
        i64_up_down_counter_with_unit!(
            "apollo.router.compute_jobs.execution.active_count",
            "Number of computation jobs in progress",
            "{job}",
            value,
            job.type = self.compute_job_type.to_string()
        );
    }
}

impl Drop for ActiveComputeMetric {
    fn drop(&mut self) {
        self.incr(-1);
    }
}

pub(super) fn observe_queue_wait_duration(
    compute_job_type: ComputeJobType,
    queue_duration: Duration,
) {
    f64_histogram_with_unit!(
        "apollo.router.compute_jobs.queue.wait.duration",
        "Time spent by the job in the compute queue",
        "s",
        queue_duration.as_secs_f64(),
        "job.type" = compute_job_type.to_string()
    );
}

pub(super) fn observe_compute_duration(compute_job_type: ComputeJobType, job_duration: Duration) {
    f64_histogram_with_unit!(
        "apollo.router.compute_jobs.execution.duration",
        "Time to execute the job, after it has been pulled from the queue",
        "s",
        job_duration.as_secs_f64(),
        "job.type" = compute_job_type.to_string()
    );
}
