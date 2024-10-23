use std::sync::Arc;
use std::sync::OnceLock;
use std::thread::JoinHandle;

use ageing::Priority;
use ageing::PriorityQueue;
use parking_lot::Condvar;
use parking_lot::Mutex;
use threadpool::ThreadPool;
use tokio::sync::oneshot;

static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();
const POOL_SIZE: usize = 8;

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
/// let output = compute_task::enqueue(|| some_heavy_computation()).await;
/// ```
pub(crate) fn enqueue<T, F>(priority: Priority, f: F) -> Result<oneshot::Receiver<T>, ageing::Error>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // https://docs.rs/threadpool/latest/threadpool/struct.Builder.html#method.num_threads
    // > defaults the number of threads to the number of CPUs
    // let pool = POOL.get_or_init(|| threadpool::Builder::new().build());
    let pool = SCHEDULER.get_or_init(|| {
        let mut scheduler = Scheduler::new();
        scheduler.start();
        scheduler
    });

    let (tx, rx) = oneshot::channel();
    pool.enqueue(priority, move || {
        let _ = tx.send(f());
    })?;
    Ok(rx)
}

/// A scheduler comprised of:
///  - a Priority Queue
///  - a Thread Pool
/// Jobs are added to the queue at a specified priority level. The scheduler removes jobs from the
/// queue and spawns them onto the thread pool.
struct Scheduler {
    queue: Arc<Mutex<PriorityQueue<()>>>,
    pool: ThreadPool,
    hdl: Option<JoinHandle<()>>,
    regulator: Arc<Condvar>,
}

impl Scheduler {
    pub(crate) fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(PriorityQueue::new())),
            pool: threadpool::Builder::new().num_threads(POOL_SIZE).build(),
            hdl: None,
            regulator: Arc::new(Condvar::new()),
        }
    }

    pub(crate) fn enqueue(
        &self,
        priority: Priority,
        task: impl FnOnce() + Send + 'static,
    ) -> Result<(), ageing::Error> {
        let mut guard = self.queue.lock();
        let is_empty = guard.is_empty();
        let result = guard.enqueue(priority, task);
        if result.is_ok() && is_empty {
            self.regulator.notify_one();
        }
        result
    }

    pub(crate) fn start(&mut self) {
        let my_queue = self.queue.clone();
        let my_pool = self.pool.clone();
        let my_regulator = self.regulator.clone();

        // Loop forever pulling jobs from our priority queue as long as we are not queueing in the
        // thread pool.
        self.hdl = Some(std::thread::spawn(move || {
            // If the thread pool is full:
            //  - wait until it is empty (clunky, but that's the threadpool interface)
            // If we have nothing in our input queue:
            //  - we must pause and wait for something to happen.
            // Note: This would be better if we could notify the regulator from the
            // threadpool when a job finishes. Until then, this is ok...
            loop {
                // Alway make sure we aren't queueing into our threadpool...
                if my_pool.active_count() == POOL_SIZE {
                    // Suspend execution until we have room for output
                    my_pool.join();
                }

                let mut guard = my_queue.lock();

                while guard.is_empty() {
                    // Suspend execution when we have no input
                    my_regulator.wait(&mut guard);
                }

                // I don't think we can fail to get a task here, but in case of racing...
                if let Some(task) = guard.dequeue() {
                    my_pool.execute(task);
                }
            }
        }));
    }
}
