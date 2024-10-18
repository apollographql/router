use std::sync::OnceLock;

use tokio::sync::oneshot;

static POOL: OnceLock<threadpool::ThreadPool> = OnceLock::new();

/// Drop-in replacement for Tokio’s [`spawn_blocking`]
/// but intended for tasks that keep a CPU core active: it uses a thread pool
/// limited to the number of available CPU cores.
///
/// `spawn_blocking` on the other hand appears to be intended for tasks that pause a thread
/// in a blocking syscall: `max_blocking_threads` is configurable but defaults to 512,
/// which is too high for tasks where the CPU is actively running.
/// Configuring it to $NUM_CPUS feels risky in case a library we use relies on `spawn_blocking`
/// in the originally intended way.
///
/// [`spawn_blocking`]: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
///
/// ## Example
///
/// Doing this in an async function can work but isn't great for Tokio performance:
///
/// ```no_run
/// let output = some_heavy_computation();
/// ```
///
/// It can be replaced with:
///
/// ```no_run
/// use crate::compute_task;
///
/// let output = compute_task::execute(|| some_heavy_computation()).await;
/// ```
pub(crate) fn execute<T, F>(f: F) -> oneshot::Receiver<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // https://docs.rs/threadpool/latest/threadpool/struct.Builder.html#method.num_threads
    // > defaults the number of threads to the number of CPUs
    let pool = POOL.get_or_init(|| threadpool::Builder::new().build());

    let (tx, rx) = oneshot::channel();
    pool.execute(move || {
        let _ = tx.send(f());
    });
    rx
}
