use std::future::Future;
use std::ops::ControlFlow;
use std::panic::UnwindSafe;
use std::sync::OnceLock;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use tokio::sync::oneshot;

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

/// We expect calling `oneshot::Sender::is_closed` to never leave the sender in a broken state.
impl<T> UnwindSafe for JobStatus<'_, T> {}

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

type Job = Box<dyn FnOnce() + Send + 'static>;

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
                    job();
                }
            });
        }
        AgeingPriorityQueue::bounded(QUEUE_SOFT_CAPACITY_PER_THREAD * pool_size)
    })
}

/// Returns a future that resolves to a `Result` that is `Ok` if `f` returned or `Err` if it panicked.
pub(crate) fn execute<T, F>(
    priority: Priority,
    job: F,
) -> Result<impl Future<Output = std::thread::Result<T>>, ComputeBackPressureError>
where
    F: FnOnce(JobStatus<'_, T>) -> T + Send + UnwindSafe + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let job = Box::new(move || {
        let status = JobStatus { result_sender: &tx };
        let result = std::panic::catch_unwind(move || job(status));
        // Ignore the error if the oneshot receiver was dropped
        let _ = tx.send(result);
    });
    let queue = queue();
    queue.send(priority, job).map_err(|e| match e {
        SendError::QueueIsFull => {
            u64_counter!(
                "apollo.router.compute_jobs.queue_is_full",
                "Number of requests rejected because the queue for compute jobs is full",
                1u64
            );
            ComputeBackPressureError
        }
        SendError::Disconnected => {
            // This never panics because this channel can never be disconnect:
            // the receiver is owned by `queue` which we can access here:
            let _proof_of_life: &'static AgeingPriorityQueue<_> = queue;
            unreachable!()
        }
    })?;
    Ok(async move {
        // This `expect` never panics because this oneshot channel can never be disconnect:
        // the sender is owned by `job` which, if we reach here, was successfully sent to the queue.
        // The queue or thread pool never drop a job without executing it.
        // When executing, `catch_unwind` ensures that the sender cannot be dropped without sending.
        rx.await.expect("channel disconnected")
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
        let job_thread = execute(Priority::P4, |_| std::thread::current().id())
            .unwrap()
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
        let one = execute(Priority::P8, |_| {
            std::thread::sleep(Duration::from_millis(1_000));
            1
        })
        .unwrap();
        let two = execute(Priority::P8, |_| {
            std::thread::sleep(Duration::from_millis(1_000));
            1 + 1
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(one.await.unwrap(), 1);
        assert_eq!(two.await.unwrap(), 2);
        // Evidence of fearless parallel sleep:
        assert!(start.elapsed() < Duration::from_millis(1_400));
    }

    #[tokio::test]
    async fn test_cancel() {
        let (side_channel_sender, side_channel_receiver) = oneshot::channel();
        let side_channel_sender = std::panic::AssertUnwindSafe(side_channel_sender);
        let queue_receiver = execute(Priority::P1, move |status| {
            // https://internals.rust-lang.org/t/assertunwindsafe-interacts-poorly-with-2021-capture-rules/21721
            let side_channel_sender = side_channel_sender;
            // We expect the first iteration to succeed,
            // but let’s add lots of margin for CI machines with super-busy CPU cores
            for _ in 0..1_000 {
                std::thread::sleep(Duration::from_millis(10));
                if status.check_for_cooperative_cancellation().is_break() {
                    side_channel_sender.0.send(Ok(())).unwrap();
                    return;
                }
            }
            side_channel_sender.0.send(Err(())).unwrap();
        });
        drop(queue_receiver);
        match side_channel_receiver.await {
            Ok(Ok(())) => {}
            e => panic!("job did not cancel as expected: {e:?}"),
        };
    }
}
