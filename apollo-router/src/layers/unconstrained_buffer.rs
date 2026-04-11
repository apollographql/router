//! A wrapper around [`Buffer`] that runs [`poll_ready`] inside
//! [`unconstrained`], preventing the cooperative budget from causing
//! a [`Poll::Pending`] yield when the inner semaphore still has capacity.
//!
//! Without this, a [`Buffer`] that sits behind a [`LoadShed`]
//! layer can be falsely shed: the Tokio coop budget reaches zero, [`poll_proceed`]
//! returns [`Pending`], and [`LoadShed`] interprets that as the service not being
//! ready, immediately returning an [`Overloaded`] error.
//!
//! By polling the inner [`Buffer`] in an unconstrained context, the coop budget
//! check is bypassed and readiness is determined solely by the actual semaphore
//! permit availability.
//!
//! ## Cases where this matters
//!
//! This only matters when a [`Buffer`] is behind a [`LoadShed`], and the problem
//! is amplified when there's another [`Buffer`] in front of that [`LoadShed`],
//! building a structure like: `Buffer(LoadShed(Buffer(service)))`.
//!
//! ### Amplification
//!
//! The amplification happens because the `Worker` loop of the outer `Buffer` picks up a message and
//! then calls [`LoadShed::poll_ready`], which always returns [`Ready`] and never
//! attempts to yield to the scheduler.
//!
//! During the [`LoadShed::poll_ready`] call, [`Buffer::poll_ready`] is called on the inner
//! `Buffer`, which is where the semaphore permit availability is checked.
//! However, before checking the semaphore, it will call [`poll_proceed`] to check coop budget
//! availability. If the coop budget is exhausted, [`poll_proceed`] will return [`Pending`],
//! which will "bubble up" to the [`LoadShed`] layer. This layer stores readiness as `false`
//! but still returns [`Ready`] to the outer `Buffer` `Worker`.
//!
//! This means that the `Worker` keeps looping and consuming coop budget until it hits the
//! coop budget check within the `poll_next_msg` which returns [`Pending`]. However, since this
//! is the top-level running task future, there's nothing absorbing this state,
//! and the `Worker` will yield to the scheduler.
//!
//! This will likely happen right after [`LoadShed`] observes a [`Buffer::poll_ready`]
//! return [`Pending`] because further calls to [`poll_proceed`] will keep returning [`Pending`]
//! until the scheduler resets the coop budget.
//!
//! On a single-threaded runtime or contended scenario, this is the moment where all accumulated
//! [`Overloaded`] errors will start to show up one after another in "waves".
//!
//! [`Pending`]: Poll::Pending
//! [`Ready`]: Poll::Ready
//! [`unconstrained`]: tokio::task::unconstrained
//! [`poll_ready`]: Service::poll_ready
//! [`Buffer::poll_ready`]: Service::poll_ready
//! [`LoadShed::poll_ready`]: Service::poll_ready
//! [`poll_proceed`]: tokio::task::coop::poll_proceed
//! [`LoadShed`]: tower::load_shed::LoadShed
//! [`Overloaded`]: tower::load_shed::error::Overloaded
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::task::Context;
use std::task::Poll;

use opentelemetry::KeyValue;
use tower::BoxError;
use tower::Layer;
use tower::buffer::Buffer;
use tower::buffer::future::ResponseFuture;
use tower_service::Service;

use crate::metrics::UpDownCounterGuard;

/// Adds a [coop unconstrained](tokio::task::unconstrained) [`Buffer`] layer to a service.
///
/// See the module documentation for more details.
#[derive(Clone)]
pub struct UnconstrainedBufferLayer<Request> {
    /// Name of the buffer layer, used for metrics.
    name: String,
    bound: usize,
    /// Buffer attributes, used for metrics.
    attributes: Vec<KeyValue>,
    _p: PhantomData<fn(Request)>,
}

impl<Request> UnconstrainedBufferLayer<Request> {
    /// Creates a new [`UnconstrainedBufferLayer`] with the provided `bound`.
    ///
    /// `bound` gives the maximal number of requests that can be queued for the service before
    /// backpressure is applied to callers.
    ///
    /// See [`Buffer::new`] for guidance on choosing a `bound`.
    pub fn new(name: impl Into<String>, bound: usize, attributes: Vec<KeyValue>) -> Self {
        UnconstrainedBufferLayer {
            name: name.into(),
            bound,
            attributes,
            _p: PhantomData,
        }
    }
}

impl<S, Request> Layer<S> for UnconstrainedBufferLayer<Request>
where
    S: Service<Request> + Send + 'static,
    S::Future: Send,
    S::Error: Into<BoxError> + Send + Sync,
    Request: Send + 'static,
{
    type Service = UnconstrainedBuffer<Request, S::Future>;

    fn layer(&self, service: S) -> Self::Service {
        UnconstrainedBuffer::new(
            self.name.clone(),
            service,
            self.bound,
            self.attributes.clone(),
        )
    }
}

impl<Request> fmt::Debug for UnconstrainedBufferLayer<Request> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("UnconstrainedBufferLayer")
            .field("name", &self.name)
            .field("bound", &self.bound)
            .finish()
    }
}

/// A wrapper around [`Buffer`] that runs [`poll_ready`] inside
/// [`unconstrained`], preventing the cooperative budget from causing
/// a [`Pending`] yield when the inner semaphore still has capacity.
///
/// See the module documentation for more details.
///
/// [`Pending`]: Poll::Pending
/// [`unconstrained`]: tokio::task::unconstrained
/// [`poll_ready`]: Service::poll_ready
#[derive(Debug)]
pub struct UnconstrainedBuffer<Req, F> {
    /// The inner [`Buffer`] layer, which wraps the actual service and is responsible for
    /// buffering requests.
    inner: Buffer<GaugedRequest<Req>, F>,
    /// Buffer attributes, used for metrics.
    attributes: Vec<KeyValue>,
}

impl<Req, F> UnconstrainedBuffer<Req, F>
where
    F: 'static,
{
    /// Creates a new `UnconstrainedBuffer` with the specified service and buffer capacity.
    pub fn new<S>(
        name: impl Into<String>,
        service: S,
        bound: usize,
        mut attributes: Vec<KeyValue>,
    ) -> Self
    where
        S: Service<Req, Future = F> + Send + 'static,
        F: Send,
        S::Error: Into<BoxError> + Send + Sync,
        Req: Send + 'static,
    {
        let inner = Buffer::new(GaugedRequestService(service), bound);
        attributes.push(KeyValue::new("layer.service.name", name.into()));
        attributes.push(KeyValue::new("buffer.capacity", bound as i64));
        Self { inner, attributes }
    }
}

impl<Req, Rsp, F, E> Service<Req> for UnconstrainedBuffer<Req, F>
where
    F: Future<Output = Result<Rsp, E>> + Send + 'static,
    E: Into<BoxError>,
    Req: Send + 'static,
{
    type Response = Rsp;
    type Error = BoxError;
    type Future = ResponseFuture<F>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        std::pin::pin!(tokio::task::unconstrained(std::future::poll_fn(|cx| {
            self.inner.poll_ready(cx)
        })))
        .as_mut()
        .poll(cx)
    }

    fn call(&mut self, request: Req) -> Self::Future {
        // Tracks the whole buffer pipeline from the moment the request is enqueued
        // to the moment it's fully processed by the Worker.
        // This means that at any given time, the counter represents the number of messages
        // currently in the buffer + 1 if there's a message being processed by the Worker.
        // In that scenario, it's completely possible to have `bound + 1` messages in flight,
        // this is a tradeoff we accept to have a guard that automatically decrements on `drop`.
        // To only track the number of messages in the buffer itself, we need to manually
        // decrement the counter in upon the first `poll_ready` call in the inner service. However,
        // that method doesn't know what request is going to be processed next.
        let counter = i64_up_down_counter_with_unit!(
            "apollo.router.buffer.messages",
            "Number of messages currently in the buffer",
            "{message}",
            1,
            self.attributes
        );
        self.inner.call(GaugedRequest(request, counter))
    }
}

impl<Req, F> Clone for UnconstrainedBuffer<Req, F>
where
    Req: Send + 'static,
    F: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            attributes: self.attributes.clone(),
        }
    }
}

/// A wrapper around the request that holds an [`UpDownCounterGuard`] to track the number of
/// messages in the buffer.
#[derive(Debug)]
struct GaugedRequest<S>(S, UpDownCounterGuard<i64>);

/// A wrapper around the inner service that accepts a [`GaugedRequest`] and forwards the inner request
/// to the actual service.
/// This is necessary because the [`Buffer`] layer operates on [`GaugedRequest`] instead of the
/// original request type, so we need to convert back before calling the inner service.
#[derive(Debug)]
struct GaugedRequestService<S>(S);

impl<S, Req> Service<GaugedRequest<Req>> for GaugedRequestService<S>
where
    S: Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: GaugedRequest<Req>) -> Self::Future {
        // Drop the `UpDownCounterGuard` after the request is processed by the inner service.

        // When the Buffer's Worker picks up a message, it first calls `poll_ready` on the inner
        // service, if the service reports `Ready`, `call` is called.
        // Here we can either:
        // 1. drop immediately to signal that the message is no longer in the
        //    buffer queue, which is not entirely correct as it's dequeued before
        //    `poll_ready` is even called, or
        // 2. Keep the guard until the message is fully processed by the Worker,
        //    which is more intuitive.
        let GaugedRequest(request, _guard) = request;
        self.0.call(request)
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::task::Poll;

    use tokio::task::JoinSet;
    use tokio::task::coop::has_budget_remaining;
    use tokio::task::coop::poll_proceed;
    use tower::BoxError;
    use tower::Service;
    use tower::load_shed::LoadShed;

    use super::*;

    /// Consumes all available computational budget in the given context until a pending state is reached.
    ///
    /// This function repeatedly polls the [`poll_proceed`] function within the provided context (`cx`)
    /// to exhaust the computational budget available and returns the total number of units consumed
    /// before reaching a pending state.
    ///
    /// # Notes
    /// - This function will loop indefinitely if [`poll_proceed`] never returns [`Poll::Pending`],
    ///   which is the case for tasks being executed in a [`tokio::task::unconstrained`] context.
    fn consume_all_budget(cx: &mut Context) -> usize {
        let mut consumed = 0;
        loop {
            let restore = poll_proceed(cx);
            match restore {
                Poll::Ready(r) => {
                    consumed += 1;
                    r.made_progress();
                    continue;
                }
                Poll::Pending => return consumed,
            }
        }
    }

    /// Deterministic test for cooperative budget exhaustion.
    ///
    /// Ensures that [`Buffer::poll_ready`] never returns [`Poll::Pending`] when the budget
    /// is exhausted. This should only happen when there are no permits available.
    #[tokio::test]
    async fn coop_budget_exhaustion_should_not_cause_buffer_poll_ready_to_return_pending() {
        // Service chain: Buffer(1000) -> inner service
        let inner = tower::service_fn(|_: ()| async { Ok::<_, BoxError>("ok") });
        let mut inner_buffered = UnconstrainedBuffer::new(inner, 1000);

        // Tries to reset the budget by yielding to the scheduler.
        tokio::task::yield_now().await;

        // Sanity check: with a fresh budget, `Buffer::poll_ready` should always succeed.
        poll_fn(|cx| {
            assert!(has_budget_remaining(), "Budget should not be exhausted");

            assert!(
                matches!(inner_buffered.poll_ready(cx), Poll::Ready(Ok(()))),
                "Buffer::poll_ready should return Ready"
            );

            // call() acquires a permit from the inner Buffer because poll_ready succeeded.
            let fut = inner_buffered.call(());
            let mut fut = std::pin::pin!(fut);

            // Ready(Ok(_)) or Pending (waiting for Buffer worker), never an error.
            assert!(
                matches!(fut.as_mut().poll(cx), Poll::Ready(Ok(_)) | Poll::Pending),
                "Buffer::call should succeed"
            );
            Poll::Ready(())
        })
        .await;

        // Tries to reset the budget by yielding to the scheduler.
        tokio::task::yield_now().await;

        // Test: buffer should not return Pending even when the coop budget is exhausted,
        // because the inner Buffer still has capacity.
        poll_fn(|cx| {
            // Drain all coop budget units via `consume_all_budget` loop.
            let budget_consumed = consume_all_budget(cx);

            assert_ne!(
                budget_consumed,
                0,
                "Expected non-zero budget units consumed"
            );

            assert!(
                !has_budget_remaining(),
                "Expected budget to be exhausted after consuming all units, but poll_proceed is still Ready"
            );

            // Budget is now 0. The inner Buffer still has 999 permits available.
            // With a constrained budget, `poll_proceed` is called and returns `Pending`
            // before `Semaphore::poll_acquire` is even called.
            // With an unconstrained budget, `poll_proceed` always returns `Ready`,
            // and `Semaphore::poll_acquire` is called normally.
            assert!(
                matches!(inner_buffered.poll_ready(cx), Poll::Ready(Ok(()))),
                "Buffer::poll_ready should return Ready even with exhausted budget"
            );
            let fut = inner_buffered.call(());
            let mut fut = std::pin::pin!(fut);

            // Ready(Ok(_)) or Pending (waiting for Buffer worker), never an error.
            assert!(
                matches!(fut.as_mut().poll(cx), Poll::Ready(Ok(_)) | Poll::Pending),
                "Buffer::call should succeed"
            );
            Poll::Ready(())
        })
            .await;
    }

    /// Deterministic test for cooperative budget exhaustion.
    ///
    /// This ensures that when the budget is exhausted, it does not cause premature shedding
    /// in the [`LoadShed`] layer when the inner [`Buffer`] still has capacity but tries
    /// to yield to the scheduler.
    #[tokio::test]
    async fn coop_budget_exhaustion_should_not_cause_false_shedding() {
        // Service chain: LoadShed -> Buffer(1000) -> instant_service
        let inner = tower::service_fn(|_: ()| async { Ok::<_, BoxError>("ok") });
        let inner_buffered = UnconstrainedBuffer::new(inner, 1000);
        let mut load_shed = LoadShed::new(inner_buffered);

        // Tries to reset the budget by yielding to the scheduler.
        tokio::task::yield_now().await;

        // Sanity check: with a fresh budget, LoadShed should not shed and Buffer should succeed.
        poll_fn(|cx| {
            assert!(has_budget_remaining(), "budget should not be exhausted");

            // Budget is fresh (128). poll_ready -> Acquire succeeds -> is_ready = true
            // `LoadShed::poll_ready` always returns `Poll::Ready`.
            assert!(
                matches!(load_shed.poll_ready(cx), Poll::Ready(Ok(()))),
                "LoadShed::poll_ready should return Ready"
            );

            // call() forwards to the inner Buffer because is_ready = true.
            let fut = load_shed.call(());
            let mut fut = std::pin::pin!(fut);

            // Ensures that load shedding didn't occur.
            assert!(
                !matches!(fut.as_mut().poll(cx), Poll::Ready(Err(_))),
                "requests should not be shed with fresh budget"
            );
            Poll::Ready(())
        })
        .await;

        // Tries to reset the budget by yielding to the scheduler.
        tokio::task::yield_now().await;

        // Test: the load should not be shed when the buffer has capacity despite the budget being
        // exhausted.
        poll_fn(|cx| {
            // Drain all coop budget units via `consume_all_budget` loop.
            let budget_consumed = consume_all_budget(cx);
            assert_ne!(
                budget_consumed,
                0,
                "Expected non-zero budget units consumed"
            );

            assert!(
                !has_budget_remaining(),
                "Expected budget to be exhausted after consuming all units, but poll_proceed is still Ready"
            );

            // `LoadShed::poll_ready` always returns `Poll::Ready`.
            assert!(
                matches!(load_shed.poll_ready(cx), Poll::Ready(Ok(()))),
                "LoadShed::poll_ready should return Ready"
            );

            let fut = load_shed.call(());
            let mut fut = std::pin::pin!(fut);

            // Overloaded resolves immediately in one poll.
            let shed = match fut.as_mut().poll(cx) {
                Poll::Ready(Err(e)) => e
                    .downcast_ref::<tower::load_shed::error::Overloaded>()
                    .is_some(),
                _ => false,
            };

            assert!(
                !shed,
                "Load should not be shed (Overloaded) when there's enough Buffer permits"
            );

            Poll::Ready(())
        })
            .await;
    }

    /// Confirms that genuine buffer exhaustion still causes [`LoadShed`] to shed requests.
    ///
    /// [`UnconstrainedBuffer`] bypasses the coop budget check but must still propagate genuine
    /// [`Poll::Pending`] from a full semaphore so that real backpressure is preserved.
    #[tokio::test]
    async fn full_buffer_should_still_cause_load_shedding() {
        use std::sync::Arc;

        use tokio::sync::Semaphore;

        // A gate that holds the inner service blocked until we release it.
        let gate = Arc::new(Semaphore::new(0));
        let gate_clone = gate.clone();

        let inner = tower::service_fn(move |_: ()| {
            let gate = gate_clone.clone();
            async move {
                // Block until explicitly released.
                let _permit = gate.acquire().await.unwrap();
                Ok::<_, BoxError>("ok")
            }
        });

        // Capacity 1: the worker holds 1 in-flight; 1 more can queue. A third makes the buffer full.
        let inner_buffered = UnconstrainedBuffer::new(inner, 1);
        let mut load_shed = LoadShed::new(inner_buffered);

        // Request 1: accepted, worker picks it up and blocks at the gate.
        // Buffer::call() enqueues synchronously; dropping the ResponseFuture only discards
        // the response receiver — the request is already in the channel.
        poll_fn(|cx| load_shed.poll_ready(cx)).await.unwrap();
        drop(load_shed.call(()));

        // Yield so the worker task runs and drains request 1 from the channel.
        tokio::task::yield_now().await;

        // Request 2: fills the channel while the worker is blocked on request 1.
        // Same as above — drop only the response receiver, not the enqueued request.
        poll_fn(|cx| load_shed.poll_ready(cx)).await.unwrap();
        drop(load_shed.call(()));

        // Request 3: the channel is now full. Buffer::poll_ready returns genuine Pending
        // (not coop-induced), LoadShed must shed this request.
        poll_fn(|cx| {
            // LoadShed::poll_ready always returns Ready — it absorbs the inner Pending.
            assert!(matches!(load_shed.poll_ready(cx), Poll::Ready(Ok(()))));

            let fut = load_shed.call(());
            let mut fut = std::pin::pin!(fut);

            // Overloaded resolves immediately in one poll.
            let is_overloaded = match fut.as_mut().poll(cx) {
                Poll::Ready(Err(e)) => e
                    .downcast_ref::<tower::load_shed::error::Overloaded>()
                    .is_some(),
                _ => false,
            };

            assert!(
                is_overloaded,
                "Expected Overloaded when buffer is genuinely full; \
                 UnconstrainedBuffer must not suppress real backpressure"
            );

            Poll::Ready(())
        })
        .await;

        // Release the gate so the worker can drain and the runtime can shut down cleanly.
        gate.add_permits(2);
    }

    /// Load-based test: ensure that shedding never happens under load with the
    /// real Buffer Worker loop.
    ///
    /// What happens under burst traffic:
    ///   1. Inner buffer fills up -> genuine [`Pending`] -> [`LoadShed`] sheds (correct).
    ///   2. Worker loops at wire speed ([`LoadShed`] always returns [`Ready`] -> never yields).
    ///   3. Each recv consumes 1 coop budget; after ~128 iterations, `budget = 0`.
    ///   4. Even when the inner buffer drains and has capacity, when `Acquire` checks
    ///      [`poll_proceed`]:
    ///      - With [constrained buffer], [`poll_proceed`] returns [`Pending`] because `budget = 0`
    ///        and the load is shed.
    ///      - With [unconstrained buffer], [`poll_proceed`] returns [`Ready`] and the semaphore
    ///        is checked normally.
    ///
    /// [`Ready`]: Poll::Ready
    /// [`Pending`]: Poll::Pending
    /// [constrained buffer]: Buffer
    /// [unconstrained buffer]: UnconstrainedBuffer
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_not_shed_under_load() {
        // How many times we iterate on the test
        let iterations: usize = 500;
        // Total number of requests per iteration
        let total_requests: usize = 100;
        // Buffer capacity.
        // Way higher than what we need, which is the total number of concurrent requests.
        let buffer_capacity = 200;

        // This can only be reliably checked with at least one layer of `LoadShed` between
        // two `Buffer` layers.
        // That's because the `outer_buffer` `Worker` will continuously call `LoadShed::poll_ready`,
        // which will never return `Poll::Pending`, therefore, never yield to the scheduler within
        // this loop.
        // This causes the `Worker` to consume all coop budget units and eventually yield from two
        // main flows:
        // 1. `poll_next_msg` call when fetching the next message in the queue.
        // 2. `poll_proceed` within `Acquire` future in an `inner_buffer` `poll_ready` call.
        // The second flow is the one that is behind a `LoadShed` layer and will cause
        // an `Overloaded` error upon an attempt of awaiting on a `Service::call` future.
        let service = tower::service_fn(move |_: ()| async move { Ok::<_, BoxError>("ok") });
        let inner_buffer = UnconstrainedBuffer::new(service, buffer_capacity);
        let load_shed = LoadShed::new(inner_buffer);
        let outer_buffer = UnconstrainedBuffer::new(load_shed, buffer_capacity);

        let mut shed = 0usize;
        let mut other_err = 0usize;
        let mut tasks = JoinSet::new();

        for _ in 0..iterations {
            // send all requests
            for _ in 0..total_requests {
                let svc = outer_buffer.clone();
                tasks.spawn(async move {
                    // Each spawned task calls ready().await then call()
                    let mut svc = svc;
                    let svc = tower::ServiceExt::ready(&mut svc).await;
                    match svc {
                        Ok(svc) => svc.call(()).await,
                        Err(e) => Err(e),
                    }
                });
            }

            // wait all spawned tasks to resolve
            while let Some(handle) = tasks.join_next().await {
                if let Err(e) = handle.expect("task panicked") {
                    if e.downcast_ref::<tower::load_shed::error::Overloaded>()
                        .is_some()
                    {
                        shed += 1;
                    } else {
                        other_err += 1;
                    }
                }
            }
        }

        assert_eq!(shed, 0, "Expected all requests to succeed without shedding");
        assert_eq!(
            other_err, 0,
            "Expected all requests to succeed without errors"
        );
    }
}
