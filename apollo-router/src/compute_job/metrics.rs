use std::time::Duration;
use std::time::Instant;

use tracing::Span;

use crate::compute_job::ComputeJobType;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;

#[derive(Copy, Clone, strum_macros::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(super) enum Outcome {
    ExecutedOk,
    ExecutedError,
    ChannelError,
    RejectedQueueFull,
    Abandoned,
}

impl From<Outcome> for opentelemetry::Value {
    fn from(outcome: Outcome) -> Self {
        let s: &'static str = outcome.into();
        s.into()
    }
}

pub(super) struct JobWatcher {
    span: Span,
    queue_start: Instant,
    compute_job_type: ComputeJobType,
    pub(super) outcome: Outcome,
}

impl JobWatcher {
    pub(super) fn new(compute_job_type: ComputeJobType) -> Self {
        Self {
            span: Span::current(),
            queue_start: Instant::now(),
            outcome: Outcome::Abandoned,
            compute_job_type,
        }
    }
}

impl Drop for JobWatcher {
    fn drop(&mut self) {
        let outcome: &'static str = self.outcome.into();
        self.span.record("job.outcome", outcome);

        match &self.outcome {
            Outcome::ExecutedOk => {
                self.span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
            }
            Outcome::ExecutedError | Outcome::ChannelError | Outcome::RejectedQueueFull => {
                self.span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            }
            _ => {}
        }
        let full_duration = self.queue_start.elapsed();
        f64_histogram_with_unit!(
            "apollo.router.compute_jobs.duration",
            "Total job processing time",
            "s",
            full_duration.as_secs_f64(),
            "job.type" = self.compute_job_type,
            "job.outcome" = outcome
        );
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
        "job.type" = compute_job_type
    );
}

pub(super) fn observe_compute_duration(compute_job_type: ComputeJobType, job_duration: Duration) {
    f64_histogram_with_unit!(
        "apollo.router.compute_jobs.execution.duration",
        "Time to execute the job, after it has been pulled from the queue",
        "s",
        job_duration.as_secs_f64(),
        "job.type" = compute_job_type
    );
}

#[cfg(test)]
mod tests {
    use crate::compute_job::ComputeJobType;
    use crate::compute_job::metrics::JobWatcher;
    use crate::compute_job::metrics::Outcome;

    #[test]
    fn test_job_watcher() {
        let check_histogram_count =
            |count: u64, job_type: &'static str, job_outcome: &'static str| {
                assert_histogram_count!(
                    "apollo.router.compute_jobs.duration",
                    count,
                    "job.type" = job_type,
                    "job.outcome" = job_outcome
                );
            };

        {
            let _job_watcher = JobWatcher::new(ComputeJobType::Introspection);
        }
        check_histogram_count(1, "introspection", "abandoned");

        {
            let mut job_watcher = JobWatcher::new(ComputeJobType::QueryParsing);
            job_watcher.outcome = Outcome::ExecutedOk;
        }
        check_histogram_count(1, "query_parsing", "executed_ok");

        for count in 1..5 {
            {
                let mut job_watcher = JobWatcher::new(ComputeJobType::QueryPlanning);
                job_watcher.outcome = Outcome::RejectedQueueFull;
            }
            check_histogram_count(count, "query_planning", "rejected_queue_full");
        }
    }
}
