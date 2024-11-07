use std::future::Future;
use std::num::NonZeroUsize;
use std::panic::UnwindSafe;
use std::sync::OnceLock;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use tokio::sync::oneshot;

use crate::ageing_priority_queue::AgeingPriorityQueue;
pub(crate) use crate::ageing_priority_queue::Priority;
use crate::metrics::meter_provider;

/// We generate backpressure in tower `poll_ready` when reaching this many queued items
// TODO: what’s a good number? should it be configurable?
const QUEUE_SOFT_CAPACITY: usize = 100;

// TODO: should this be configurable?
fn thread_pool_size() -> NonZeroUsize {
    std::thread::available_parallelism().expect("available_parallelism() failed")
}

type Job = Box<dyn FnOnce() + Send + 'static>;

fn queue() -> &'static AgeingPriorityQueue<Job> {
    static QUEUE: OnceLock<AgeingPriorityQueue<Job>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        for _ in 0..thread_pool_size().get() {
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
        AgeingPriorityQueue::soft_bounded(QUEUE_SOFT_CAPACITY)
    })
}

/// Returns a future that resolves to a `Result` that is `Ok` if `f` returned or `Err` if it panicked.
pub(crate) fn execute<T, F>(
    priority: Priority,
    job: F,
) -> impl Future<Output = std::thread::Result<T>>
where
    F: FnOnce() -> T + Send + UnwindSafe + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let job = Box::new(move || {
        // Ignore the error if the oneshot receiver was dropped
        let _ = tx.send(std::panic::catch_unwind(job));
    });
    queue().send(priority, job);
    async { rx.await.expect("channel disconnected") }
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
