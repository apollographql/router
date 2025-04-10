mod metrics;

use std::future::Future;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::Instant;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use tokio::sync::oneshot;

use self::metrics::ActiveComputeMetric;
use self::metrics::JobWatcher;
use self::metrics::Outcome;
use self::metrics::observe_compute_duration;
use self::metrics::observe_queue_wait_duration;
use crate::ageing_priority_queue::AgeingPriorityQueue;
pub(crate) use crate::ageing_priority_queue::Priority;
use crate::ageing_priority_queue::SendError;
use crate::metrics::meter_provider;

/// We generate backpressure in tower `poll_ready` when the number of queued jobs
/// reaches `QUEUE_SOFT_CAPACITY_PER_THREAD * thread_pool_size()`
const QUEUE_SOFT_CAPACITY_PER_THREAD: usize = 20;

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

pub(crate) struct JobStatus<'a, T> {
    result_sender: &'a oneshot::Sender<std::thread::Result<T>>,
}

impl<T> JobStatus<'_, T> {
    /// Checks whether the oneshot receiver for the result of the job was dropped,
    /// which means nothing is expecting the result anymore.
    ///
    /// This can happen if the Tokio task owning it is cancelled,
    /// such as if a supergraph client disconnects or if a request times out.
    ///
    /// In this case, a long-running job should try to cancel itself
    /// to avoid needless resource consumption.
    pub(crate) fn check_for_cooperative_cancellation(&self) -> ControlFlow<()> {
        if self.result_sender.is_closed() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

/// Compute job queue is full
#[derive(thiserror::Error, Debug, displaydoc::Display, Clone)]
pub(crate) struct ComputeBackPressureError;

#[derive(Debug)]
pub(crate) enum MaybeBackPressureError<E> {
    /// Doing the same request again later would result in the same error (e.g. invalid query).
    ///
    /// This error can be cached.
    PermanentError(E),

    /// Doing the same request again later might work.
    ///
    /// This error must not be cached.
    TemporaryError(ComputeBackPressureError),
}

impl<E> From<E> for MaybeBackPressureError<E> {
    fn from(error: E) -> Self {
        Self::PermanentError(error)
    }
}

impl ComputeBackPressureError {
    pub(crate) fn to_graphql_error(&self) -> crate::graphql::Error {
        crate::graphql::Error::builder()
            .message("Your request has been concurrency limited during query processing")
            .extension_code("REQUEST_CONCURRENCY_LIMITED")
            .build()
    }
}

impl crate::graphql::IntoGraphQLErrors for ComputeBackPressureError {
    fn into_graphql_errors(self) -> Result<Vec<crate::graphql::Error>, Self> {
        Ok(vec![self.to_graphql_error()])
    }
}

#[derive(Copy, Clone, Hash, Eq, PartialEq, Debug, strum_macros::IntoStaticStr)]
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
    ty: ComputeJobType,
    queue_start: Instant,
    job_fn: Box<dyn FnOnce() + Send + 'static>,
}

pub(crate) fn queue() -> &'static AgeingPriorityQueue<Job> {
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
                    let job = receiver.blocking_recv();
                    observe_queue_wait_duration(job.ty, job.queue_start.elapsed());

                    let _active_metric = ActiveComputeMetric::register(job.ty);
                    let job_start = Instant::now();
                    (job.job_fn)();
                    observe_compute_duration(job.ty, job_start.elapsed());
                }
            });
        }
        AgeingPriorityQueue::bounded(QUEUE_SOFT_CAPACITY_PER_THREAD * pool_size)
    })
}

/// Returns a future that resolves to a `Result` that is `Ok` if `f` returned or `Err` if it panicked.
pub(crate) fn execute<T, F>(
    compute_job_type: ComputeJobType,
    job: F,
) -> Result<impl Future<Output = T>, ComputeBackPressureError>
where
    F: FnOnce(JobStatus<'_, T>) -> T + Send + 'static,
    T: Send + 'static,
{
    let mut job_watcher = JobWatcher::new(compute_job_type);

    let (tx, rx) = oneshot::channel();
    let wrapped_job_fn = Box::new(move || {
        let status = JobStatus { result_sender: &tx };
        // `AssertUnwindSafe` here is correct because this `catch_unwind`
        // is paired with `resume_unwind` below, so the overall effect on unwind safety
        // is the same as if the caller had executed `job` directly without a thread pool.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(status)));
        match tx.send(result) {
            Ok(()) => {}
            Err(_) => {
                // `rx` was dropped: `result` is no longer needed and we can safely drop it
            }
        }
    });

    let queue = queue();
    let job = Job {
        ty: compute_job_type,
        job_fn: wrapped_job_fn,
        queue_start: Instant::now(),
    };

    queue
        .send(Priority::from(compute_job_type), job)
        .map_err(|e| match e {
            SendError::QueueIsFull => {
                u64_counter!(
                    "apollo.router.compute_jobs.queue_is_full",
                    "Number of requests rejected because the queue for compute jobs is full",
                    1u64
                );
                job_watcher.outcome = Outcome::RejectedQueueFull;
                ComputeBackPressureError
            }
            SendError::Disconnected => {
                // This never panics because this channel can never be disconnect:
                // the receiver is owned by `queue` which we can access here:
                let _proof_of_life: &'static AgeingPriorityQueue<_> = queue;
                unreachable!("compute thread pool queue is disconnected")
            }
        })?;

    Ok(async move {
        let result = rx.await;
        job_watcher.outcome = match &result {
            Ok(Ok(_)) => Outcome::ExecutedOk,
            // We don't know what the cardinality of errors are so we just say there was a response error
            Ok(Err(_)) => Outcome::ExecutedError,
            // We got an error reading the response from the channel
            Err(_) => Outcome::ChannelError,
        };

        match result {
            Ok(Ok(value)) => value,
            Ok(Err(panic_payload)) => {
                // The `job` callback panicked.
                //
                // We try to to avoid this (by returning errors instead) and consider this a bug.
                // But if it does happen, propagating the panic to the caller from here
                // has the same effect as if they had executed `job` directly
                // without a thread pool.
                //
                // Additionally we have a panic handler in `apollo-router/src/executable.rs`
                // that exits the process,
                // so in practice a Router thread should never start unwinding
                // an this code path should be unreachable.
                std::panic::resume_unwind(panic_payload)
            }
            Err(e) => {
                let _: tokio::sync::oneshot::error::RecvError = e;
                // This should never happen because this oneshot channel can never be disconnect:
                // the sender is owned by `job` which, if we reach here,
                // was successfully sent to the queue.
                // The queue or thread pool never drop a job without executing it.
                // When executing, `catch_unwind` ensures that
                // the sender cannot be dropped without sending.
                unreachable!("compute result oneshot channel is disconnected")
            }
        }
    })
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
        let job_thread = execute(ComputeJobType::QueryParsing, |_| {
            std::thread::current().id()
        })
        .unwrap()
        .await;
        assert_ne!(job_thread, test_thread)
    }

    #[tokio::test]
    async fn test_parallelism() {
        if thread_pool_size() < 2 {
            return;
        }
        let start = Instant::now();
        let one = execute(ComputeJobType::QueryPlanning, |_| {
            std::thread::sleep(Duration::from_millis(1_000));
            1
        })
        .unwrap();
        let two = execute(ComputeJobType::QueryPlanning, |_| {
            std::thread::sleep(Duration::from_millis(1_000));
            1 + 1
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(one.await, 1);
        assert_eq!(two.await, 2);
        // Evidence of fearless parallel sleep:
        assert!(start.elapsed() < Duration::from_millis(1_400));
    }

    #[tokio::test]
    async fn test_cancel() {
        let (side_channel_sender, side_channel_receiver) = oneshot::channel();
        let queue_receiver = execute(ComputeJobType::Introspection, move |status| {
            // We expect the first iteration to succeed,
            // but let’s add lots of margin for CI machines with super-busy CPU cores
            for _ in 0..1_000 {
                std::thread::sleep(Duration::from_millis(10));
                if status.check_for_cooperative_cancellation().is_break() {
                    side_channel_sender.send(Ok(())).unwrap();
                    return;
                }
            }
            side_channel_sender.send(Err(())).unwrap();
        });
        drop(queue_receiver);
        match side_channel_receiver.await {
            Ok(Ok(())) => {}
            e => panic!("job did not cancel as expected: {e:?}"),
        };
    }
}
