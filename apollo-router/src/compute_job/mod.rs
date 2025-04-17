mod metrics;

use std::any::Any;
use std::future::Future;
use std::panic::UnwindSafe;
use std::sync::OnceLock;
use std::time::Instant;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use tokio::sync::oneshot;
use tracing::Instrument;
use tracing::Span;
use tracing::info_span;
use tracing_core::Dispatch;
use tracing_subscriber::util::SubscriberInitExt;

use self::metrics::ActiveComputeMetric;
use self::metrics::JobWatcher;
use self::metrics::Outcome;
use self::metrics::observe_compute_duration;
use self::metrics::observe_queue_wait_duration;
use crate::ageing_priority_queue::AgeingPriorityQueue;
use crate::ageing_priority_queue::Priority;
use crate::metrics::meter_provider;
use crate::plugins::telemetry::consts::COMPUTE_JOB_EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::consts::COMPUTE_JOB_SPAN_NAME;

/// We generate backpressure in tower `poll_ready` when the number of queued jobs
/// reaches `APOLLO_ROUTER_COMPUTE_QUEUE_CAPACITY_PER_THREAD * thread_pool_size()`
///
/// The default for APOLLO_ROUTER_COMPUTE_QUEUE_CAPACITY_PER_THREAD is 1000
///
/// This number is somewhat arbitrary and subject to change. Most compute jobs
/// don't take a long time, so by making the queue quite big, it's capable of eating
/// a sizable backlog during spikes.
fn queue_capacity() -> usize {
    // This environment variable is intentionally undocumented.
    std::env::var("APOLLO_ROUTER_COMPUTE_QUEUE_CAPACITY_PER_THREAD")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1000)
}

/// By default, let this thread pool use all available resources if it can.
/// In the worst case, we’ll have moderate context switching cost
/// as the kernel’s scheduler distributes time to it or Tokio or other threads.
fn thread_pool_size() -> usize {
    // This environment variable is intentionally undocumented.
    if let Some(threads) = std::env::var("APOLLO_ROUTER_COMPUTE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        threads
    } else {
        std::thread::available_parallelism()
            .expect("available_parallelism() failed")
            .get()
    }
}

#[derive(Copy, Clone, Hash, Eq, PartialEq, Debug, strum_macros::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum ComputeJobType {
    QueryParsing,
    QueryPlanning,
    Introspection,
}

impl From<ComputeJobType> for Priority {
    fn from(job_type: ComputeJobType) -> Self {
        match job_type {
            ComputeJobType::QueryPlanning => Self::P8, // high
            ComputeJobType::QueryParsing => Self::P4,  // medium
            ComputeJobType::Introspection => Self::P1, // low
        }
    }
}

impl From<ComputeJobType> for opentelemetry::Value {
    fn from(compute_job_type: ComputeJobType) -> Self {
        let s: &'static str = compute_job_type.into();
        s.into()
    }
}

pub(crate) struct Job {
    subscriber: Dispatch,
    parent_span: Span,
    ty: ComputeJobType,
    queue_start: Instant,
    job_fn: Box<dyn FnOnce() + Send + 'static>,
}

fn queue() -> &'static AgeingPriorityQueue<Job> {
    static QUEUE: OnceLock<AgeingPriorityQueue<Job>> = OnceLock::new();
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
                    let (job, age) = receiver.blocking_recv();
                    let job_type: &'static str = job.ty.into();
                    let age: &'static str = age.into();
                    let _subscriber = job.subscriber.set_default();
                    job.parent_span.in_scope(|| {
                        let span = info_span!(
                            COMPUTE_JOB_EXECUTION_SPAN_NAME,
                            "job.type" = job_type,
                            "job.age" = age
                        );
                        span.in_scope(|| {
                            observe_queue_wait_duration(job.ty, job.queue_start.elapsed());

                            let _active_metric = ActiveComputeMetric::register(job.ty);
                            let job_start = Instant::now();
                            (job.job_fn)();
                            observe_compute_duration(job.ty, job_start.elapsed());
                        })
                    })
                }
            });
        }
        AgeingPriorityQueue::soft_bounded(queue_capacity() * pool_size)
    })
}

/// Returns a future that resolves to a `Result` that is `Ok` if `f` returned or `Err` if it panicked.
pub(crate) fn execute<T, F>(
    compute_job_type: ComputeJobType,
    job: F,
) -> impl Future<Output = std::thread::Result<T>>
where
    F: FnOnce() -> T + Send + UnwindSafe + 'static,
    T: Send + 'static,
{
    let compute_job_type_str: &'static str = compute_job_type.into();
    let span = info_span!(
        COMPUTE_JOB_SPAN_NAME,
        "job.type" = compute_job_type_str,
        "job.outcome" = tracing::field::Empty
    );
    span.in_scope(|| {
        let job_watcher = JobWatcher::new(compute_job_type);

        let (tx, rx) = oneshot::channel();
        let job = Box::new(move || {
            // Ignore the error if the oneshot receiver was dropped
            let _ = tx.send(std::panic::catch_unwind(job));
        });

        let job = Job {
            subscriber: Dispatch::default(),
            parent_span: Span::current(),
            ty: compute_job_type,
            job_fn: job,
            queue_start: Instant::now(),
        };
        queue().send(compute_job_type.into(), job);

        async move {
            let result = rx.await;
            // This local variable MUST exist
            let mut local_job_watcher = job_watcher;
            local_job_watcher.outcome = match &result {
                Ok(Ok(_)) => Outcome::ExecutedOk,
                Ok(Err(_)) => Outcome::ExecutedError,
                Err(_) => Outcome::ChannelError,
            };
            match result {
                Ok(r) => r,
                Err(e) => Err(Box::new(e) as Box<dyn Any + Send>),
            }
        }
        .in_current_span()
    })
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

    use tracing_futures::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;

    #[tokio::test]
    async fn test_observability() {
        // In this test we expect the logged message to have

        async {
            let span = info_span!("test_observability");
            async {
                tracing::info!("Outer");
                let job = execute(ComputeJobType::QueryParsing, || {
                    tracing::info!("Inner");
                    1
                });
                let result = job.await.unwrap();
                assert_eq!(result, 1);
            }
            .instrument(span)
            .await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }

    #[tokio::test]
    async fn test_executes_on_different_thread() {
        let test_thread = std::thread::current().id();
        let job_thread = execute(ComputeJobType::QueryParsing, || std::thread::current().id())
            .await
            .expect("job panicked");
        assert_ne!(job_thread, test_thread)
    }

    #[tokio::test]
    async fn test_parallelism() {
        if thread_pool_size() < 2 {
            return;
        }
        let start = Instant::now();
        let one = execute(ComputeJobType::QueryPlanning, || {
            std::thread::sleep(Duration::from_millis(1_000));
            1
        });
        let two = execute(ComputeJobType::QueryPlanning, || {
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
