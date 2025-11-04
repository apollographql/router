mod metrics;

use std::any::Any;
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::Instant;

use apollo_federation::error::FederationError;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use tokio::sync::oneshot;
use tracing::Instrument;
use tracing::Span;
use tracing::info_span;
use tracing_core::Dispatch;
use tracing_subscriber::util::SubscriberInitExt;

use self::metrics::JobWatcher;
use self::metrics::Outcome;
use self::metrics::observe_compute_duration;
use self::metrics::observe_queue_wait_duration;
use crate::ageing_priority_queue::AgeingPriorityQueue;
use crate::ageing_priority_queue::Priority;
use crate::ageing_priority_queue::SendError;
use crate::allocator::AllocationLimit;
use crate::allocator::current;
use crate::metrics::meter_provider;
use crate::plugins::telemetry::consts::COMPUTE_JOB_EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::consts::COMPUTE_JOB_SPAN_NAME;

/// We generate backpressure in tower `poll_ready` when the number of queued jobs
/// reaches `QUEUE_SOFT_CAPACITY_PER_THREAD * thread_pool_size()`
///
/// This number is somewhat arbitrary and subject to change. Most compute jobs
/// don't take a long time, so by making the queue quite big, it's capable of eating
/// a sizable backlog during spikes.
const QUEUE_SOFT_CAPACITY_PER_THREAD: usize = 1_000;

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
            println!("result sender is closed");
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

/// Job was cancelled due to cooperative cancellation
#[derive(thiserror::Error, Debug, displaydoc::Display, Clone)]
pub(crate) struct ComputeCooperativeCancellationError;

impl ComputeCooperativeCancellationError {
    pub(crate) fn to_graphql_error(&self) -> crate::graphql::Error {
        crate::graphql::Error::builder()
            .message("Your request has been cancelled due to cooperative cancellation")
            .extension_code("REQUEST_COOPERATIVE_CANCELLATION")
            .build()
    }
}

impl crate::graphql::IntoGraphQLErrors for ComputeCooperativeCancellationError {
    fn into_graphql_errors(self) -> Result<Vec<crate::graphql::Error>, Self> {
        Ok(vec![self.to_graphql_error()])
    }
}

#[derive(Copy, Clone, Hash, Eq, PartialEq, Debug, strum_macros::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum ComputeJobType {
    QueryParsing,
    QueryPlanning,
    Introspection,
    QueryParsingWarmup,
    QueryPlanningWarmup,
}

impl From<ComputeJobType> for Priority {
    fn from(job_type: ComputeJobType) -> Self {
        match job_type {
            ComputeJobType::QueryPlanning => Self::P8,       // high
            ComputeJobType::QueryParsing => Self::P4,        // medium
            ComputeJobType::Introspection => Self::P3,       // low
            ComputeJobType::QueryParsingWarmup => Self::P1,  // low
            ComputeJobType::QueryPlanningWarmup => Self::P2, // low
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
    allocation_stats: Option<std::sync::Arc<crate::allocator::AllocationStats>>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<usize>>,
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

                            let _active_metric = i64_up_down_counter_with_unit!(
                                "apollo.router.compute_jobs.active_jobs",
                                "Number of computation jobs in progress",
                                "{job}",
                                1,
                                job.type = job.ty
                            );
                            let job_start = Instant::now();

                            // Execute job with memory tracking if stats are available
                            if let Some(stats) = job.allocation_stats {
                                let max_bytes =
                                    std::env::var("APOLLO_ROUTER_QUERY_PLANNER_MEMORY_LIMIT")
                                        .ok()
                                        .and_then(|s| s.parse::<usize>().ok());

                                // Create a child context with the job type as the name
                                let job_name: &'static str = job.ty.into();

                                crate::allocator::with_parented_memory_tracking(
                                    job_name,
                                    stats,
                                    || {
                                        (job.job_fn)();
                                        if let Some(allocation_stats) = current() {
                                            record_metrics(&allocation_stats);
                                        }
                                    },
                                    Option::zip(max_bytes, job.cancel_tx).map(
                                        |(max_bytes, sender)| {
                                            AllocationLimit::new(max_bytes, sender)
                                        },
                                    ),
                                );
                            } else {
                                (job.job_fn)();
                            }
                            observe_compute_duration(job.ty, job_start.elapsed());
                        })
                    })
                }
            });
        }
        tracing::info!(
            threads = pool_size,
            queue_capacity = QUEUE_SOFT_CAPACITY_PER_THREAD * pool_size,
            "compute job thread pool created",
        );
        AgeingPriorityQueue::bounded(QUEUE_SOFT_CAPACITY_PER_THREAD * pool_size)
    })
}

fn record_metrics(stats: &crate::allocator::AllocationStats) {
    let bytes_allocated = stats.bytes_allocated() as u64;
    let bytes_deallocated = stats.bytes_deallocated() as u64;
    let bytes_zeroed = stats.bytes_zeroed() as u64;
    let bytes_reallocated = stats.bytes_reallocated() as u64;
    let context_name = stats.name();

    // Record total bytes allocated
    u64_histogram_with_unit!(
        "apollo.router.query_planner.memory",
        "Memory allocated during query planning",
        "By",
        bytes_allocated,
        allocation.type = "allocated",
        context = context_name
    );

    // Record bytes deallocated
    u64_histogram_with_unit!(
        "apollo.router.query_planner.memory",
        "Memory allocated during query planning",
        "By",
        bytes_deallocated,
        allocation.type = "deallocated",
        context = context_name
    );

    // Record bytes zeroed
    u64_histogram_with_unit!(
        "apollo.router.query_planner.memory",
        "Memory allocated during query planning",
        "By",
        bytes_zeroed,
        allocation.type = "zeroed",
        context = context_name
    );

    // Record bytes reallocated
    u64_histogram_with_unit!(
        "apollo.router.query_planner.memory",
        "Memory allocated during query planning",
        "By",
        bytes_reallocated,
        allocation.type = "reallocated",
        context = context_name
    );
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
    let compute_job_type_str: &'static str = compute_job_type.into();
    let span = info_span!(
        COMPUTE_JOB_SPAN_NAME,
        "job.type" = compute_job_type_str,
        "job.outcome" = tracing::field::Empty
    );
    span.in_scope(|| {
        let mut job_watcher = JobWatcher::new(compute_job_type);
        let (tx, mut rx) = oneshot::channel();

        let is_cancellable = crate::allocator::current().is_some();

        let (cancel_tx, cancel_rx) = if is_cancellable {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };

        let wrapped_job_fn = Box::new(move || {
            let status = JobStatus { result_sender: &tx };
            // `AssertUnwindSafe` here is correct because this `catch_unwind`
            // is paired with `resume_unwind` below, so the overall effect on unwind safety
            // is the same as if the caller had executed `job` directly without a thread pool.
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(status)));
            match tx.send(result) {
                Ok(()) => {}
                Err(_) => {
                    // `rx` was dropped: `result` is no longer needed and we can safely drop it
                }
            }
        });

        let queue = queue();
        let job = Job {
            subscriber: Dispatch::default(),
            parent_span: Span::current(),
            ty: compute_job_type,
            job_fn: wrapped_job_fn,
            queue_start: Instant::now(),
            allocation_stats: crate::allocator::current(),
            cancel_tx,
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
            let result: Result<Result<T, Box<dyn Any + Send>>, oneshot::error::RecvError> =
                if let Some(mut cancel_rx) = cancel_rx {
                    println!("waiting for cancel or result");
                    tokio::select! {
                        biased;
                        cancellation = &mut cancel_rx => {
                            if let Ok(bytes_requested) = cancellation {
                                // TODO(memory-tracking): How can we get the operation name here?
                                tracing::error!("job {compute_job_type_str} cancelled as it exceeded memory limit (requested {bytes_requested} bytes)");
                            }
                            Ok(Err(Box::new(ComputeCooperativeCancellationError)))
                        }
                        result = &mut rx => result
                    }
                } else {
                    rx.await
                };

            // This local variable MUST exist. Otherwise, only the field from the JobWatcher struct is moved and drop will occur before the outcome is set.
            // This is predicated on all the fields in the struct being Copy!!!
            let mut local_job_watcher = job_watcher;
            local_job_watcher.outcome = match &result {
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
        }
        .in_current_span())
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

    use tracing_futures::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;

    /// Send a request to the compute queue to make sure it is initialized.
    ///
    /// The queue is (a) wrapped in a `OnceLock`, so it is shared between tests, and (b) only
    /// initialized after receiving and processing a request.
    /// These two properties can lead to inconsistent behavior.
    async fn ensure_queue_is_initialized() {
        execute(ComputeJobType::Introspection, |_| {})
            .unwrap()
            .await;
    }

    #[tokio::test]
    async fn test_observability() {
        // make sure that the queue has been initialized - if this step is skipped, the
        // queue will _sometimes_ be initialized in the step below, which causes an
        // additional log line and a snapshot mismatch.
        ensure_queue_is_initialized().await;

        async {
            let span = info_span!("test_observability");
            let job = span.in_scope(|| {
                tracing::info!("Outer");
                execute(ComputeJobType::QueryParsing, |_| {
                    tracing::info!("Inner");
                    1
                })
                .unwrap()
            });
            let result = job.await;
            assert_eq!(result, 1);
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }

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
