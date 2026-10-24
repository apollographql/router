//! A service handle whose clone is a reference-count increment.
//!
//! Many layers clone the service beneath them on every call, so that the copy they made ready
//! can move into the response future. Cloning a [`BoxCloneService`] clones every layer beneath
//! it, so without a boundary each such clone copies the rest of the pipeline. [`PooledService`]
//! is that boundary: it shares a pool of copies of the service beneath it, and a copy is only
//! made when every pooled copy is in use.
//!
//! Unlike a [`Buffer`], it adds no worker task or channel, and readiness still comes from the
//! service beneath: [`poll_ready`] takes a copy from the pool and polls it, and [`call`] calls
//! that copy and puts it back.
//!
//! [`BoxCloneService`]: tower::util::BoxCloneService
//! [`Buffer`]: tower::buffer::Buffer
//! [`poll_ready`]: Service::poll_ready
//! [`call`]: Service::call

use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use parking_lot::Mutex;
use tower::Service;

/// A cloneable handle to a pool of copies of a service. See the [module documentation](self).
pub(crate) struct PooledService<S> {
    pool: Arc<Pool<S>>,
    /// The copy [`poll_ready`](Service::poll_ready) took from the pool, until it is called.
    ready: Option<S>,
}

struct Pool<S> {
    /// Locked only to clone a new copy, so taking and returning idle copies never waits on a
    /// clone. `S` isn't `Sync`, so even cloning through `&S` needs the lock.
    prototype: Mutex<S>,
    idle: Mutex<Vec<S>>,
    max_idle: usize,
}

impl<S: Clone> Pool<S> {
    fn take(&self) -> S {
        let idle = self.idle.lock().pop();
        idle.unwrap_or_else(|| self.prototype.lock().clone())
    }

    fn put_back(&self, service: S) {
        let mut idle = self.idle.lock();
        if idle.len() < self.max_idle {
            idle.push(service);
        }
    }
}

/// Idle copies kept per available CPU.
///
/// On the synchronous path a copy is out of the pool only between `poll_ready` and `call` on
/// one worker thread, so a few per thread cover steady load. Layers that await between the
/// two, such as a router-stage coprocessor, can hold many more at once; keeping all of those
/// after a burst would hold that many full stacks until the next reload. Copies beyond the
/// cap are dropped, and a later burst clones them again, as every call did before this pool.
const IDLE_COPIES_PER_CPU: usize = 8;

impl<S> PooledService<S> {
    pub(crate) fn new(service: S) -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, usize::from);
        Self::with_max_idle(service, cpus * IDLE_COPIES_PER_CPU)
    }

    fn with_max_idle(service: S, max_idle: usize) -> Self {
        Self {
            pool: Arc::new(Pool {
                prototype: Mutex::new(service),
                idle: Mutex::new(Vec::new()),
                max_idle,
            }),
            ready: None,
        }
    }
}

impl<S> Clone for PooledService<S> {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            ready: None,
        }
    }
}

impl<S, Request> Service<Request> for PooledService<S>
where
    S: Service<Request> + Clone,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let service = self.ready.get_or_insert_with(|| self.pool.take());
        let poll = service.poll_ready(cx);
        if let Poll::Ready(Err(_)) = poll {
            // A service that failed readiness may not be usable again.
            self.ready = None;
        }
        poll
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Some callers in the pipeline call without polling readiness, which its services
        // tolerate because they are always ready; they get a copy that was not polled, as
        // they did before this boundary.
        let mut service = self.ready.take().unwrap_or_else(|| self.pool.take());
        let future = service.call(request);
        // Only a called copy goes back. A copy dropped while ready may hold capacity it
        // reserved in `poll_ready`, such as a concurrency-limit permit, which an idle copy
        // must not keep.
        self.pool.put_back(service);
        future
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use futures::FutureExt;
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::*;
    use crate::layers::ServiceBuilderExt;

    /// An always-ready service that counts how many times it has been cloned.
    #[derive(Default)]
    struct CountsClones {
        clones: Arc<AtomicUsize>,
    }

    impl Clone for CountsClones {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                clones: Arc::clone(&self.clones),
            }
        }
    }

    impl Service<()> for CountsClones {
        type Response = ();
        type Error = BoxError;
        type Future = futures::future::Ready<Result<(), BoxError>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: ()) -> Self::Future {
            futures::future::ready(Ok(()))
        }
    }

    /// Three layers that each clone the service beneath them on every call.
    fn three_cloning_layers<S>(service: S) -> impl Service<(), Response = (), Error = BoxError>
    where
        S: Service<(), Response = (), Error = BoxError> + Clone + Send + 'static,
        S::Future: Send,
    {
        ServiceBuilder::new()
            .checkpoint_async(|()| async { Ok(ControlFlow::Continue(())) })
            .checkpoint_async(|()| async { Ok(ControlFlow::Continue(())) })
            .checkpoint_async(|()| async { Ok(ControlFlow::Continue(())) })
            .service(service)
    }

    async fn call_times(service: &mut impl Service<(), Response = (), Error = BoxError>, n: usize) {
        for _ in 0..n {
            service.ready().await.unwrap().call(()).await.unwrap();
        }
    }

    #[test]
    fn cloning_the_handle_does_not_clone_the_service() {
        let inner = CountsClones::default();
        let clones = Arc::clone(&inner.clones);
        let pooled = PooledService::new(inner);

        let handles: Vec<_> = (0..10).map(|_| pooled.clone()).collect();

        assert_eq!(handles.len(), 10);
        assert_eq!(clones.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn sequential_calls_reuse_one_copy() {
        let inner = CountsClones::default();
        let clones = Arc::clone(&inner.clones);
        let mut service = three_cloning_layers(PooledService::new(inner));

        call_times(&mut service, 10).await;

        // The first call made the one copy the pool hands out from then on.
        assert_eq!(clones.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn without_the_boundary_every_cloning_layer_copies_the_service() {
        let inner = CountsClones::default();
        let clones = Arc::clone(&inner.clones);
        let mut service = three_cloning_layers(inner);

        call_times(&mut service, 10).await;

        assert_eq!(clones.load(Ordering::Relaxed), 30);
    }

    #[tokio::test]
    async fn readiness_comes_from_the_service_beneath() {
        let (mock, mut handle) = tower_test::mock::pair::<(), ()>();
        let mut pooled = PooledService::new(mock);

        handle.allow(0);
        assert!(pooled.ready().now_or_never().is_none());

        handle.allow(1);
        pooled.ready().await.unwrap();
        let response = pooled.call(());
        handle
            .next_request()
            .await
            .expect("the request reaches the service beneath")
            .1
            .send_response(());
        response.await.unwrap();
    }

    #[tokio::test]
    async fn idle_copies_beyond_the_cap_are_dropped() {
        let pooled = PooledService::with_max_idle(CountsClones::default(), 2);
        let mut handles: Vec<_> = (0..5).map(|_| pooled.clone()).collect();

        for handle in &mut handles {
            handle.ready().await.unwrap();
        }
        for handle in &mut handles {
            handle.call(()).await.unwrap();
        }

        assert_eq!(pooled.pool.idle.lock().len(), 2);
    }

    #[tokio::test]
    async fn a_copy_dropped_while_ready_is_not_reused() {
        let inner = CountsClones::default();
        let clones = Arc::clone(&inner.clones);
        let pooled = PooledService::new(inner);

        let mut ready_then_dropped = pooled.clone();
        ready_then_dropped.ready().await.unwrap();
        drop(ready_then_dropped);
        call_times(&mut pooled.clone(), 1).await;

        assert_eq!(clones.load(Ordering::Relaxed), 2);
    }
}
