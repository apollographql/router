use std::time::Duration;
use std::time::Instant;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use tracing::Span;

use crate::compute_job::ComputeJobType;
use crate::metrics::meter_provider;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;

#[derive(Copy, Clone, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(super) enum Outcome {
    ExecutedOk,
    ExecutedError,
    ChannelError,
    RejectedQueueFull,
    Abandoned,
}

impl_otel_value_from_static_str!(Outcome);

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

fn create_queue_size_gauge() -> ObservableGauge<u64> {
    meter_provider()
        .meter("apollo/router")
        .u64_observable_gauge("apollo.router.compute_jobs.queued")
        .with_description(
            "Number of computation jobs (parsing, planning, …) waiting to be scheduled",
        )
        .with_callback(move |m| m.observe(super::queue().queued_count() as u64, &[]))
        .build()
}

/// A pass-through layer that handles the lifecycle of compute job telemetry.
///
/// This must be created once the telemetry plugin is already activated.
#[derive(Clone)]
pub(crate) struct ComputeJobMetricsLayer {
    queue_size_gauge: ObservableGauge<u64>,
}

impl ComputeJobMetricsLayer {
    pub(crate) fn new() -> Self {
        Self {
            queue_size_gauge: create_queue_size_gauge(),
        }
    }
}

impl<S> tower::Layer<S> for ComputeJobMetricsLayer {
    type Service = ComputeJobMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ComputeJobMetricsService {
            inner,
            _queue_size_gauge: self.queue_size_gauge.clone(),
        }
    }
}

/// A pass-through service that just exists to manage the lifecycle of compute job metric
/// instruments.
#[derive(Clone)]
pub(crate) struct ComputeJobMetricsService<S> {
    inner: S,
    _queue_size_gauge: ObservableGauge<u64>,
}

impl<Req, S> tower::Service<Req> for ComputeJobMetricsService<S>
where
    S: tower::Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        self.inner.call(req)
    }
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
