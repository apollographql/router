use std::time::Duration;
use std::time::Instant;

use crate::compute_job::ComputeJobType;

#[derive(Copy, Clone, strum_macros::IntoStaticStr)]
pub(super) enum Outcome {
    Executed,
    ExecutedErrorResponse,
    ExecutedErrorChannel,
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
        let full_duration = self.queue_start.elapsed();
        f64_histogram_with_unit!(
            "apollo.router.compute_jobs.duration",
            "Total job processing time",
            "s",
            full_duration.as_secs_f64(),
            "job.type" = self.compute_job_type,
            "job.outcome" = self.outcome
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
            job.type = self.compute_job_type
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
    use crate::compute_job::metrics::ActiveComputeMetric;
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
        check_histogram_count(1, "Introspection", "Abandoned");

        {
            let mut job_watcher = JobWatcher::new(ComputeJobType::QueryParsing);
            job_watcher.outcome = Outcome::Executed;
        }
        check_histogram_count(1, "QueryParsing", "Executed");

        for count in 1..5 {
            {
                let mut job_watcher = JobWatcher::new(ComputeJobType::QueryPlanning);
                job_watcher.outcome = Outcome::RejectedQueueFull;
            }
            check_histogram_count(count, "QueryPlanning", "RejectedQueueFull");
        }
    }

    #[test]
    fn test_active_compute_metric() {
        let check_count = |count: i64, job_type: &'static str| {
            assert_up_down_counter!(
                "apollo.router.compute_jobs.execution.active_count",
                count,
                "job.type" = job_type
            );
        };

        {
            let _introspection_1 = ActiveComputeMetric::register(ComputeJobType::Introspection);
            let _introspection_2 = ActiveComputeMetric::register(ComputeJobType::Introspection);
            let introspection_3 = ActiveComputeMetric::register(ComputeJobType::Introspection);
            check_count(3, "Introspection");

            let _planning_1 = ActiveComputeMetric::register(ComputeJobType::QueryPlanning);
            check_count(3, "Introspection");
            check_count(1, "QueryPlanning");

            drop(introspection_3);
            check_count(2, "Introspection");
            check_count(1, "QueryPlanning");
        }

        // block ended, so should have no ongoing computation
        check_count(0, "Introspection");
        check_count(0, "QueryPlanning");
    }
}
