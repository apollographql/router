use std::future::Future;
use std::panic::UnwindSafe;
use std::sync::atomic::AtomicUsize;
use std::sync::OnceLock;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use strum_macros::Display;
use tokio::sync::oneshot;
use tracing::field;
use tracing_futures::Instrument;

use crate::ageing_priority_queue::AgeingPriorityQueue;
pub(crate) use crate::ageing_priority_queue::Priority;
use crate::metrics::meter_provider;
use crate::plugins::telemetry::consts::WORKER_POOL_SPAN_NAME;

/// We generate backpressure in tower `poll_ready` when the number of queued jobs
/// reaches `QUEUE_SOFT_CAPACITY_PER_THREAD * thread_pool_size()`
const QUEUE_SOFT_CAPACITY_PER_THREAD: usize = 20;

static CONFIGURED_POOL_SIZE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn experimental_set_thread_pool_size(size: usize) {
    CONFIGURED_POOL_SIZE.store(size, std::sync::atomic::Ordering::Release);
}

/// Let this thread pool use all available resources if it can.
/// In the worst case, we’ll have moderate context switching cost
/// as the kernel’s scheduler distributes time to it or Tokio or other threads.
fn thread_pool_size() -> usize {
    let mut configured_size = CONFIGURED_POOL_SIZE.load(std::sync::atomic::Ordering::Acquire);
    if configured_size == 0 {
        configured_size = std::thread::available_parallelism()
            .expect("available_parallelism() failed")
            .get();
    }
    tracing::info!(
        configured_size = configured_size,
        "starting worker pool with size"
    );
    configured_size
}

#[derive(Display, Copy, Clone)]
pub(crate) enum ComputeJobType {
    QueryParsing,
    QueryPlanning,
    Introspection,
}

struct ComputeJob {
    ty: ComputeJobType,
    priority: Priority,
    job: Box<dyn FnOnce() + Send + 'static>,
    parent_span: tracing::Span,
    queue_start: std::time::Instant,
}

fn queue() -> &'static AgeingPriorityQueue<ComputeJob> {
    static QUEUE: OnceLock<AgeingPriorityQueue<ComputeJob>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let pool_size = thread_pool_size();
        for _ in 0..pool_size {
            std::thread::spawn(|| {
                // This looks like we need the queue before creating the queue,
                // but it happens in a child thread where OnceLock will block
                // until `get_or_init` in the parent thread is finished
                // and the parent is *not* blocked on the child thread making progress.
                let queue = queue();

                let mut receiver = queue.receiver();
                loop {
                    let (job, priority) = receiver.blocking_recv();
                    let queue_duration = job.queue_start.elapsed();
                    f64_histogram!(
                        "apollo.router.compute_jobs.queue.wait.duration",
                        "Number of seconds it took to queue the job",
                        queue_duration.as_millis() as f64 / 1000.0f64,
                        "job.type" = job.ty.to_string(),
                        "job.priority" = job.priority.to_string()
                    );
                    let job_start = std::time::Instant::now();
                    let _guard = job.parent_span.enter();
                    (job.job)();
                    let job_duration = job_start.elapsed();
                    f64_histogram!(
                        "apollo.router.compute_jobs.execution.duration",
                        "Number of seconds it took to execute the job",
                        job_duration.as_millis() as f64 / 1000.0f64,
                        "job.type" = job.ty.to_string(),
                        "job.priority" = job.priority.to_string()
                    );
                }
            });
        }
        AgeingPriorityQueue::soft_bounded(QUEUE_SOFT_CAPACITY_PER_THREAD * pool_size)
    })
}

/// Returns a future that resolves to a `Result` that is `Ok` if `f` returned or `Err` if it panicked.
pub(crate) fn execute<T, F>(
    priority: Priority,
    compute_job_type: ComputeJobType,
    job: F,
) -> impl Future<Output = std::thread::Result<T>>
where
    F: FnOnce() -> T + Send + UnwindSafe + 'static,
    T: Send + 'static,
{
    let mut job_watcher = JobWatcher {
        queue_start: std::time::Instant::now(),
        outcome: Outcome::Abandoned,
        compute_job_type,
    };
    let worker_pool_span = tracing::info_span!(
        WORKER_POOL_SPAN_NAME,
        "otel.kind" = "INTERNAL",
        "job.priority" = priority.to_string(),
        "job.outcome" = field::Empty
    );
    let (tx, rx) = oneshot::channel();
    let job = Box::new(move || {
        // Ignore the error if the oneshot receiver was dropped
        let _ = tx.send(std::panic::catch_unwind(job));
    });
    let job = ComputeJob {
        ty: compute_job_type,
        priority,
        job,
        parent_span: worker_pool_span.clone(),
        queue_start: std::time::Instant::now(),
    };
    queue().send(priority, job);
    async move {
        let result = rx
            .instrument(worker_pool_span)
            .await
            .expect("channel disconnected");
        job_watcher.outcome = Outcome::Executed;

        // TODO update the span...
        result
    }
}

#[derive(Display)]
enum Outcome {
    Executed,
    Abandoned,
}

struct JobWatcher {
    queue_start: std::time::Instant,
    outcome: Outcome,
    compute_job_type: ComputeJobType,
}

impl Drop for JobWatcher {
    fn drop(&mut self) {
        tracing::Span::current().record("job.outcome", &self.outcome.to_string());
        let compute_job_type = self.compute_job_type.to_string();
        let outcome = self.outcome.to_string();
        let priority = self.compute_job_type.to_string();
        u64_counter!(
            "apollo.router.compute_jobs.queue.jobs",
            "Information about the jobs",
            1,
            "job.type" = compute_job_type,
            "job.priority" = priority,
            //"job.priority.final" = priority, // TODO the final priority if the job was executed
            "job.outcome" = outcome
        );
    }
}

pub(crate) fn is_full() -> bool {
    queue().is_full()
}

pub(crate) fn create_queue_size_gauge() -> ObservableGauge<u64> {
    meter_provider()
        .meter("apollo/router")
        .u64_observable_gauge("apollo.router.compute_jobs.queued")
        .with_description(
            "Number of computation jobs (parsing, planning, …) waiting to be scheduled",
        )
        .with_callback(move |m| m.observe(queue().queued_count() as u64, &[]))
        .init()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn test_executes_on_different_thread() {
        let test_thread = std::thread::current().id();
        let job_thread = execute(Priority::P4, ComputeJobType::QueryParsing, || {
            std::thread::current().id()
        })
        .await
        .unwrap();
        assert_ne!(job_thread, test_thread)
    }

    #[tokio::test]
    async fn test_parallelism() {
        if thread_pool_size() < 2 {
            return;
        }
        let start = Instant::now();
        let one = execute(Priority::P8, ComputeJobType::QueryParsing, || {
            std::thread::sleep(Duration::from_millis(1_000));
            1
        });
        let two = execute(Priority::P8, ComputeJobType::QueryParsing, || {
            std::thread::sleep(Duration::from_millis(1_000));
            1 + 1
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(one.await.unwrap(), 1);
        assert_eq!(two.await.unwrap(), 2);
        // Evidence of fearless parallel sleep:
        assert!(start.elapsed() < Duration::from_millis(1_400));
    }
}
