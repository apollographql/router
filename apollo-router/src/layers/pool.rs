//! A service shared by every caller without a worker task: a pool of copies of it.
//!
//! A tower `Buffer` shares a service by running every caller's `poll_ready` and `call` on its one
//! worker task, with a channel send, a oneshot and two task wakes per call, so the synchronous work
//! of every layer beneath it runs on that task, one call after another. A [`Pooled`] handle holds
//! copies of the service instead: a call takes an idle copy, readies it and calls it on the
//! caller's own task, and puts it back before awaiting the response. Readiness still comes from the
//! service beneath, and nothing runs on another task.
//!
//! A copy is cloned from the prototype only when every copy is out. A service that is out of the
//! pool only between its readiness and its call meets that at its first calls on each thread, so
//! a pool holds a few copies per thread under steady load.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::task::ready;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use pin_project_lite::pin_project;
use tokio::sync::AcquireError;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tower::Service;

/// Idle copies a pool keeps per available core. A copy is out of the pool from its readiness to
/// its call, so a few per thread cover steady load. A service whose readiness waits, behind a
/// concurrency limit for example, can have many copies out at once in a burst, and a pool that
/// kept every copy returned would hold that many until the pipeline is rebuilt; copies returned
/// past the cap are dropped.
const IDLE_COPIES_PER_CORE: usize = 8;

/// Copies of a service, taken and returned by the calls of a [`Pooled`] handle.
struct Pool<S> {
    /// The service every copy is cloned from. It is locked only to clone it: a service need not
    /// be `Sync`, so even a clone takes the lock, and taking or returning a copy never waits on
    /// one.
    prototype: Mutex<S>,
    idle: Mutex<Vec<S>>,
    max_idle: usize,
}

impl<S: Clone> Pool<S> {
    fn new(service: S) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        Self {
            prototype: Mutex::new(service),
            idle: Mutex::new(Vec::new()),
            max_idle: cores * IDLE_COPIES_PER_CORE,
        }
    }

    /// An idle copy, or a new one where none is idle.
    fn take(&self) -> S {
        let idle = self.idle.lock().pop();
        idle.unwrap_or_else(|| self.prototype.lock().clone())
    }

    /// Returns a copy that was called, kept while the pool holds fewer idle copies than its cap.
    ///
    /// A copy that was readied and not called is never returned, since its readiness may hold
    /// what it reserved, such as a concurrency limit's permit, which would then stay reserved
    /// until the copy's next call.
    fn put_back(&self, service: S) {
        let mut idle = self.idle.lock();
        if idle.len() < self.max_idle {
            idle.push(service);
        }
    }
}

/// A cloneable handle to a pool of copies of a service, itself a service. Its future takes a copy,
/// readies it on the caller's task, calls it and returns it to the pool before awaiting the
/// response, so the handle is always ready. Cloning the handle clones no copy.
///
/// At most `bound` calls wait for a copy's readiness at once, and the next waits for one of them
/// to be called, holding no copy meanwhile, never shed: the bound a buffer's queue holds over the
/// calls waiting for its worker.
pub(crate) struct Pooled<S> {
    pool: Arc<Pool<S>>,
    waiting: Arc<Semaphore>,
}

impl<S: Clone> Pooled<S> {
    /// A handle letting at most `bound` calls wait for a copy's readiness at once.
    pub(crate) fn bounded(service: S, bound: usize) -> Self {
        Self {
            pool: Arc::new(Pool::new(service)),
            waiting: Arc::new(Semaphore::new(bound)),
        }
    }
}

impl<S> Clone for Pooled<S> {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            waiting: Arc::clone(&self.waiting),
        }
    }
}

impl<S, Request> Service<Request> for Pooled<S>
where
    S: Service<Request> + Clone,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = PooledFuture<S, Request>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // `try_acquire` first, so a call with room under the bound costs no future
        match Arc::clone(&self.waiting).try_acquire_owned() {
            Ok(permit) => PooledFuture::Readying {
                copy: Some(self.pool.take()),
                request: Some(request),
                pool: Arc::clone(&self.pool),
                permit: Some(permit),
            },
            Err(_) => PooledFuture::Admitting {
                waiting: Arc::clone(&self.waiting),
                acquiring: None,
                request: Some(request),
                pool: Arc::clone(&self.pool),
            },
        }
    }
}

pin_project! {
    /// The future of a call through [`Pooled`]: its admission, where the bound is full; its copy
    /// readied and called; then the call's own future. A copy whose readiness fails, or whose
    /// future is dropped before it is ready, is dropped and not returned to the pool.
    #[project = PooledProjection]
    pub(crate) enum PooledFuture<S, Request>
    where
        S: Service<Request>,
    {
        Admitting {
            waiting: Arc<Semaphore>,
            acquiring: Option<BoxFuture<'static, Result<OwnedSemaphorePermit, AcquireError>>>,
            request: Option<Request>,
            pool: Arc<Pool<S>>,
        },
        Readying {
            copy: Option<S>,
            request: Option<Request>,
            pool: Arc<Pool<S>>,
            permit: Option<OwnedSemaphorePermit>,
        },
        Called {
            #[pin]
            future: S::Future,
        },
    }
}

impl<S, Request> Future for PooledFuture<S, Request>
where
    S: Service<Request> + Clone,
{
    type Output = Result<S::Response, S::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().project() {
                PooledProjection::Admitting {
                    waiting,
                    acquiring,
                    request,
                    pool,
                } => {
                    let acquiring = acquiring
                        .get_or_insert_with(|| Box::pin(Arc::clone(waiting).acquire_owned()));
                    let permit = ready!(acquiring.as_mut().poll(cx))
                        .expect("the bound's semaphore is never closed");
                    let readying = PooledFuture::Readying {
                        copy: Some(pool.take()),
                        request: request.take(),
                        pool: Arc::clone(pool),
                        permit: Some(permit),
                    };
                    self.set(readying);
                }
                PooledProjection::Readying {
                    copy,
                    request,
                    pool,
                    permit,
                } => {
                    let service = copy.as_mut().expect("a future polled after completion");
                    ready!(service.poll_ready(cx))?;
                    let mut service = copy.take().expect("the copy readied");
                    let future = service.call(request.take().expect("a request called once"));
                    pool.put_back(service);
                    // called: the next call past the bound may take its place
                    drop(permit.take());
                    self.set(PooledFuture::Called { future });
                }
                PooledProjection::Called { future } => return future.poll(cx),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use futures::FutureExt;
    use futures::task::AtomicWaker;
    use tower::BoxError;
    use tower::ServiceExt;

    use super::*;

    /// A service answering its request and recording the task of each of its readiness checks and
    /// calls, counting the copies made of it, whose readiness waits until the gate is opened and
    /// fails once where `fail_once` is set.
    #[derive(Default)]
    struct Gate {
        copies: Arc<AtomicUsize>,
        open: Arc<AtomicBool>,
        opened: Arc<AtomicWaker>,
        fail_once: Arc<AtomicBool>,
        tasks: Arc<Mutex<Vec<Option<tokio::task::Id>>>>,
    }

    impl Gate {
        fn new(open: bool) -> Self {
            let gate = Self::default();
            gate.open.store(open, Ordering::Relaxed);
            gate
        }

        fn open(&self) {
            self.open.store(true, Ordering::Relaxed);
            self.opened.wake();
        }
    }

    impl Clone for Gate {
        fn clone(&self) -> Self {
            self.copies.fetch_add(1, Ordering::Relaxed);
            Self {
                copies: self.copies.clone(),
                open: self.open.clone(),
                opened: self.opened.clone(),
                fail_once: self.fail_once.clone(),
                tasks: self.tasks.clone(),
            }
        }
    }

    impl Service<u32> for Gate {
        type Response = u32;
        type Error = BoxError;
        type Future = std::future::Ready<Result<u32, BoxError>>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            self.tasks.lock().push(tokio::task::try_id());
            if self.fail_once.swap(false, Ordering::Relaxed) {
                return Poll::Ready(Err("not ready".into()));
            }
            self.opened.register(cx.waker());
            if self.open.load(Ordering::Relaxed) {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }

        fn call(&mut self, request: u32) -> Self::Future {
            self.tasks.lock().push(tokio::task::try_id());
            std::future::ready(Ok(request))
        }
    }

    #[tokio::test]
    async fn a_call_readies_and_calls_its_copy_on_the_callers_task() {
        let gate = Gate::new(true);
        let tasks = gate.tasks.clone();
        let pooled = Pooled::bounded(gate, 1);

        let caller = tokio::spawn(async move {
            let answer = pooled.oneshot(7).await.expect("an answer");
            (answer, tokio::task::id())
        });
        let (answer, caller) = caller.await.expect("the caller");

        assert_eq!(answer, 7);
        assert_eq!(*tasks.lock(), vec![Some(caller), Some(caller)]);
    }

    #[tokio::test]
    async fn past_the_bound_a_call_waits_holding_no_copy_until_a_waiting_call_is_called() {
        let gate = Gate::new(false);
        let copies = gate.copies.clone();
        let pooled = Pooled::bounded(gate.clone(), 1);
        let made = copies.load(Ordering::Relaxed);

        let mut first = pooled.clone().oneshot(1).boxed();
        let mut second = pooled.clone().oneshot(2).boxed();
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        let copies_while_waiting = copies.load(Ordering::Relaxed) - made;

        gate.open();
        assert_eq!(first.await.expect("the first answer"), 1);
        assert_eq!(second.await.expect("the second answer"), 2);

        assert_eq!(
            (copies_while_waiting, copies.load(Ordering::Relaxed) - made),
            (1, 1),
            "the copies made with the first call waiting for readiness and the second for \
             admission, and after both answered"
        );
    }

    #[tokio::test]
    async fn a_readiness_error_fails_its_own_call_alone() {
        let gate = Gate::new(true);
        gate.fail_once.store(true, Ordering::Relaxed);
        let copies = gate.copies.clone();
        let pooled = Pooled::bounded(gate, 1);

        let first = pooled.clone().oneshot(1).await;
        let second = pooled.clone().oneshot(2).await;

        assert_eq!(
            (
                first.map_err(|error| error.to_string()),
                second.map_err(|error| error.to_string()),
                copies.load(Ordering::Relaxed),
            ),
            (Err("not ready".to_string()), Ok(2), 2),
            "each call's answer, and the copies made: the failed copy is not returned"
        );
    }
}
