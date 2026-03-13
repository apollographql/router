//! Instrumented buffer layer for the router, which wraps tower's Buffer layer and provides metrics about the buffer state.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::buffer::Buffer;
use tower::buffer::future::ResponseFuture;
use tower_service::Service;

use crate::metrics::NoopGuard;
use crate::metrics::UpDownCounterGuard;

// ─── Outer wrapper: increments when poll_ready reserves a permit ─────────────

/// Instrumented buffer layer for the router, which wraps tower's Buffer layer and provides metrics about the buffer state.
pub struct InstrumentedBuffer<Req, F> {
    /// The inner buffer layer, which wraps the actual service and is responsible for buffering requests.
    inner: Buffer<Req, F>,
    /// Shared gauge for tracking the buffer state, including reserved permits and tasks in the buffer.
    gauge: BufferGauge,
    /// Tracks whether `poll_ready` has reserved a permit that hasn't
    /// been consumed by `call` yet.  Used to avoid double-counting
    /// and to decrement on drop.
    permit_reservation: Option<BufferGaugeReservationGuard>,
}

impl<Req, F> InstrumentedBuffer<Req, F>
where
    F: 'static,
{
    /// Creates a new instrumented buffer layer with the given name, extra attributes, service, and buffer bound.
    pub fn new<S>(
        name: impl Into<String>,
        extra: Vec<(String, String)>,
        service: S,
        bound: usize,
    ) -> Self
    where
        S: Service<Req, Future = F> + Send + 'static,
        F: Send,
        S::Error: Into<BoxError> + Send + Sync,
        Req: Send + 'static,
    {
        let attrs = attrs(name.into(), extra);
        let gauge = BufferGauge::new(attrs);

        // Wrap the inner service so it decrements on worker dequeue
        let gauged_inner = GaugedInner {
            inner: service,
            gauge: gauge.clone(),
            processing: None,
        };

        let inner = Buffer::new(gauged_inner, bound);

        Self {
            inner,
            gauge,
            permit_reservation: None,
        }
    }
}

impl<Req, Rsp, F, E> Service<Req> for InstrumentedBuffer<Req, F>
where
    F: Future<Output = Result<Rsp, E>> + Send + 'static,
    E: Into<BoxError>,
    Req: Send + 'static,
{
    type Response = Rsp;
    type Error = BoxError;
    type Future = ResponseFutureGuard<F>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.inner.poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                self.permit_reservation.get_or_insert_with(|| {
                    // Channel permit has been reserved by PollSender::poll_reserve
                    self.gauge.reserve()
                });
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
                self.gauge.reject(true);
                Poll::Ready(Err(e))
            }
            Poll::Pending => {
                self.gauge.reject(false);
                Poll::Pending
            }
        }
    }

    fn call(&mut self, request: Req) -> Self::Future {
        // now the permit is actually in use
        // send_item consumes the reservation — permit is now
        // held by the message sitting in the channel.
        // Count stays the same; it decrements when the worker dequeues.

        // Here we only decrement the reservation number.
        drop(self.permit_reservation.take());

        // We count the task awaiting to be picked up by the worker,
        // which will be decremented in the worker when the task is polled.
        self.gauge.awaiting();

        // We create a guard for the task in the buffer,
        // which will be held until the response future is complete.
        let guard = self.gauge.task();
        let fut = self.inner.call(request);
        ResponseFutureGuard::new(guard, fut)
    }
}

impl<Req, F> Clone for InstrumentedBuffer<Req, F>
where
    Req: Send + 'static,
    F: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            gauge: self.gauge.clone(),
            // New clone has its own PollSender with no reservation
            permit_reservation: None,
        }
    }
}

impl<Req, F> Drop for InstrumentedBuffer<Req, F> {
    fn drop(&mut self) {
        if self.permit_reservation.take().is_some() {
            self.gauge.dropped();
        }
    }
}

// ─── Gauge Prepare Guard ─────────────────────────────────────────────────────

/// Gauge reservation guard for the buffer.
#[derive(Debug)]
#[allow(dead_code)]
struct BufferGaugeReservationGuard(UpDownCounterGuard<i64>);

/// Gauge permit guard for the buffer.
#[derive(Debug)]
#[allow(dead_code)]
struct BufferGaugePermitGuard(UpDownCounterGuard<i64>);

/// Gauge processing guard for the buffer worker.
#[derive(Debug)]
#[allow(dead_code)]
struct BufferGaugeProcessingGuard(UpDownCounterGuard<i64>);

// ─── Inner wrapper: decrements when the worker dequeues and calls ────────────

struct GaugedInner<S> {
    inner: S,
    gauge: BufferGauge,
    processing: Option<BufferGaugeProcessingGuard>,
}

impl<S, Req> Service<Req> for GaugedInner<S>
where
    S: Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // By the worker implementation, whenever this is called,
        // we have polled a message out of the channel.
        self.processing.get_or_insert_with(|| {
            // Decrements the awaiting count, and increments the processing count.
            // This is done here because this guard is guaranteed to live until the task being
            // processed by the worker is complete.
            self.gauge.done_awaiting();
            self.gauge.process()
        });
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Req) -> Self::Future {
        // This is only called when the inner service is ready,
        // which means that the answer will be sent back to the sender and the message will
        // be removed from the worker.
        drop(self.processing.take());
        self.inner.call(request)
    }
}

// ─── Layer ───────────────────────────────────────────────────────────────────

/// Instrumented buffer layer for the router, which wraps tower's Buffer layer and provides metrics about the buffer state.
pub struct InstrumentedBufferLayer<Request> {
    bound: usize,
    name: String,
    extra: Vec<(String, String)>,
    _p: PhantomData<fn(Request)>,
}

impl<Request> InstrumentedBufferLayer<Request> {
    /// Creates a new instrumented buffer layer with the given name, extra attributes, and buffer bound.
    pub fn new(name: impl Into<String>, extra: Vec<(String, String)>, bound: usize) -> Self {
        Self {
            bound,
            name: name.into(),
            extra,
            _p: PhantomData,
        }
    }
}

fn attrs(name: String, extra: Vec<(String, String)>) -> Vec<opentelemetry::KeyValue> {
    std::iter::once(opentelemetry::KeyValue::new("buffer.name", name))
        .chain(
            extra
                .into_iter()
                .map(|(k, v)| opentelemetry::KeyValue::new(k, v)),
        )
        .collect()
}

impl<S, Request> Layer<S> for InstrumentedBufferLayer<Request>
where
    S: Service<Request> + Send + 'static,
    S::Future: Send,
    S::Error: Into<BoxError> + Send + Sync,
    Request: Send + 'static,
{
    type Service = InstrumentedBuffer<Request, S::Future>;

    fn layer(&self, service: S) -> Self::Service {
        InstrumentedBuffer::new(self.name.clone(), self.extra.clone(), service, self.bound)
    }
}

impl<Request> Clone for InstrumentedBufferLayer<Request> {
    fn clone(&self) -> Self {
        Self {
            bound: self.bound,
            name: self.name.clone(),
            extra: self.extra.clone(),
            _p: PhantomData,
        }
    }
}

impl<Request> fmt::Debug for InstrumentedBufferLayer<Request> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstrumentedBufferLayer")
            .field("bound", &self.bound)
            .field("name", &self.name)
            .finish()
    }
}

// ─── Shared gauge ────────────────────────────────────────────────────────────

/// Shared gauge for tracking the buffer state, including reserved permits and tasks in the buffer.
#[derive(Debug, Clone)]
pub struct BufferGauge {
    attrs: Vec<opentelemetry::KeyValue>,
}

impl BufferGauge {
    /// Creates a new buffer gauge with the given attributes.
    pub fn new(attrs: Vec<opentelemetry::KeyValue>) -> Self {
        Self { attrs }
    }

    fn reserve(&self) -> BufferGaugeReservationGuard {
        let up_down = i64_up_down_counter_with_unit!(
            "apollo.router.buffer.permits",
            "Number of buffer permits reserved",
            "{permit}",
            1,
            self.attrs
        );
        BufferGaugeReservationGuard(up_down)
    }

    fn task(&self) -> BufferGaugePermitGuard {
        let up_down = i64_up_down_counter_with_unit!(
            "apollo.router.buffer.task",
            "Number of tasks in the buffer",
            "{task}",
            1,
            self.attrs
        );
        BufferGaugePermitGuard(up_down)
    }

    fn awaiting(&self) {
        metric!(
            i64,
            up_down_counter,
            NoopGuard,
            add,
            "apollo.router.buffer.awaiting",
            "Number of tasks awaiting for the buffer work to pick up",
            "{permit}",
            1,
            self.attrs
        );
    }

    fn done_awaiting(&self) {
        metric!(
            i64,
            up_down_counter,
            NoopGuard,
            add,
            "apollo.router.buffer.awaiting",
            "Number of tasks awaiting for the buffer work to pick up",
            "{permit}",
            -1,
            self.attrs
        );
    }

    fn process(&self) -> BufferGaugeProcessingGuard {
        let up_down = i64_up_down_counter_with_unit!(
            "apollo.router.buffer.worker.processing",
            "Number of tasks being processed by the buffer worker",
            "{permit}",
            1,
            self.attrs
        );
        BufferGaugeProcessingGuard(up_down)
    }

    fn dropped(&self) {
        u64_counter_with_unit!(
            "apollo.router.buffer.reservation.dropped",
            "Number of buffer dropped with pending reservations",
            "{permit}",
            1,
            self.attrs
        );
    }

    fn reject(&self, errored: bool) {
        let new_attrs = self
            .attrs
            .iter()
            .cloned()
            .chain(std::iter::once(opentelemetry::KeyValue::new(
                "buffer.rejection.errored",
                errored.to_string(),
            )))
            .collect::<Vec<_>>();
        u64_counter_with_unit!(
            "apollo.router.buffer.rejection",
            "Number of buffer permits rejected due to buffer being full or closed",
            "{rejection}",
            1,
            new_attrs
        );
    }
}

// ─── Guard in progress task ───────────────────────────────────────

pin_project! {
    /// Future that completes when the buffered service eventually services the submitted request.
    #[derive(Debug)]
    pub struct ResponseFutureGuard<T> {
        permit: BufferGaugePermitGuard,
        #[pin]
        inner: ResponseFuture<T>,
    }
}

impl<T> ResponseFutureGuard<T> {
    fn new(permit: BufferGaugePermitGuard, inner: ResponseFuture<T>) -> Self {
        ResponseFutureGuard { permit, inner }
    }
}

impl<F, T, E> Future for ResponseFutureGuard<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<BoxError>,
{
    type Output = Result<T, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::Poll;

    use futures::future::poll_fn;
    use tower::Service;

    use super::InstrumentedBuffer;

    // A simple always-ready service that echoes its input.
    #[derive(Clone)]
    struct EchoService;

    impl Service<u32> for EchoService {
        type Response = u32;
        type Error = tower::BoxError;
        type Future = std::future::Ready<Result<u32, tower::BoxError>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: u32) -> Self::Future {
            std::future::ready(Ok(req))
        }
    }

    /// Demonstrates the spurious-shed mechanism: `LoadShed` calls `poll_ready` once on the
    /// buffer service and uses the result to decide whether to forward or shed the request.
    /// Because `Buffer::poll_ready` delegates to `PollSender::poll_reserve`, and every fresh
    /// `PollSender` clone starts in `State::Idle`, the first poll transitions to `Acquiring`
    /// and must await `reserve_owned()`.  Tokio's semaphore `Acquire` future calls
    /// `poll_proceed` (the cooperative scheduling budget check) before attempting the acquire.
    /// When there is no pre-allocated task budget — as is the case for a bare `poll_fn` call
    /// that simulates `LoadShed`'s single-shot `poll_ready` — `poll_proceed` returns
    /// `Poll::Pending` immediately, causing the entire chain to return `Pending` even though
    /// the channel has ample capacity.
    ///
    /// This test quantifies that rate: with a 50,000-slot buffer and zero in-flight requests,
    /// a single `poll_ready` call per fresh clone returns `Pending` on the vast majority of
    /// iterations.  `LoadShed` records `is_ready = false` on every such `Pending`, then sheds
    /// the next `call()` as overloaded — a spurious HTTP 429.
    #[tokio::test]
    async fn load_shed_sees_pending_on_first_poll_with_ample_capacity() {
        const BOUND: usize = 50_000;
        const ITERS: usize = 10_000;

        let pending_count = Arc::new(AtomicUsize::new(0));
        let ready_count = Arc::new(AtomicUsize::new(0));

        for _ in 0..ITERS {
            // Each iteration gets a fresh InstrumentedBuffer clone, matching the tower
            // service-per-request pattern used in the router.
            let mut buf: InstrumentedBuffer<u32, _> =
                InstrumentedBuffer::new("test", vec![], EchoService, BOUND);

            let pending = pending_count.clone();
            let ready = ready_count.clone();

            // poll_fn with no surrounding `.await` chain mirrors what LoadShed does:
            // it polls the inner service once and immediately records the result.
            poll_fn(|cx| {
                match buf.poll_ready(cx) {
                    Poll::Pending => {
                        pending.fetch_add(1, Ordering::Relaxed);
                        Poll::Ready(()) // don't retry; LoadShed doesn't either
                    }
                    Poll::Ready(_) => {
                        ready.fetch_add(1, Ordering::Relaxed);
                        Poll::Ready(())
                    }
                }
            })
            .await;
        }

        let pending = pending_count.load(Ordering::Relaxed);
        let ready = ready_count.load(Ordering::Relaxed);
        let pct = pending * 100 / ITERS;

        println!(
            "Single-shot poll_ready over {ITERS} iterations (bound={BOUND}, 0 in-flight): \
             Ready={ready}, Pending={pending} ({pct}%)"
        );

        // The buffer has 50,000 free slots. Pending is caused entirely by Tokio's cooperative
        // scheduling budget check inside the semaphore Acquire future, not by backpressure.
        // We assert that more than half of calls return Pending to confirm the mechanism is
        // active; the observed rate is typically >95%.
        assert!(
            pending > ITERS / 2,
            "Expected most poll_ready calls to return Pending due to coop budget (not capacity). \
             Got only {pending}/{ITERS} Pending — mechanism may have changed."
        );
    }

    /// Confirms the same spurious-Pending mechanism is present under real concurrency,
    /// with multiple tasks sharing one buffer.  Each task clones the buffer per iteration
    /// (matching the router's per-request clone pattern) and polls exactly once — the
    /// same sequence LoadShed executes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn load_shed_sees_pending_under_concurrent_load() {
        const BOUND: usize = 50_000;
        const TASKS: usize = 200;
        const ITERS_PER_TASK: usize = 500;

        let pending_count = Arc::new(AtomicUsize::new(0));
        let ready_count = Arc::new(AtomicUsize::new(0));

        let base: InstrumentedBuffer<u32, _> =
            InstrumentedBuffer::new("test_concurrent", vec![], EchoService, BOUND);

        let mut handles = Vec::with_capacity(TASKS);
        for _ in 0..TASKS {
            let mut buf = base.clone();
            let pending = pending_count.clone();
            let ready = ready_count.clone();

            handles.push(tokio::spawn(async move {
                for _ in 0..ITERS_PER_TASK {
                    poll_fn(|cx| match buf.poll_ready(cx) {
                        Poll::Pending => {
                            pending.fetch_add(1, Ordering::Relaxed);
                            Poll::Ready(()) // don't retry; mirroring LoadShed behaviour
                        }
                        Poll::Ready(_) => {
                            ready.fetch_add(1, Ordering::Relaxed);
                            // Consume the permit so the next iteration starts with a fresh clone.
                            let _ = buf.call(0);
                            Poll::Ready(())
                        }
                    })
                    .await;
                }
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }

        let pending = pending_count.load(Ordering::Relaxed);
        let ready = ready_count.load(Ordering::Relaxed);
        let total = TASKS * ITERS_PER_TASK;
        let pct = pending * 100 / total;

        println!(
            "Concurrent poll_ready ({TASKS} tasks × {ITERS_PER_TASK} iters = {total}, \
             bound={BOUND}): Ready={ready}, Pending={pending} ({pct}%)"
        );

        assert!(
            pending > total / 2,
            "Expected majority of poll_ready calls to return Pending. \
             Got {pending}/{total} — mechanism may have changed."
        );
    }

    /// Control test: confirms that `Poll::Pending` does *not* appear when the buffer is
    /// polled correctly — i.e. retried after `Pending` until `Ready`, as a well-behaved
    /// caller (not `LoadShed`) would do.  This rules out the buffer being fundamentally
    /// broken and isolates the problem to `LoadShed`'s single-shot poll contract.
    #[tokio::test]
    async fn well_behaved_caller_always_succeeds_with_ample_capacity() {
        const BOUND: usize = 50_000;
        const ITERS: usize = 1_000;

        let base: InstrumentedBuffer<u32, _> =
            InstrumentedBuffer::new("test_wellbehaved", vec![], EchoService, BOUND);

        for _ in 0..ITERS {
            let mut buf = base.clone();
            // tower::ServiceExt::ready() retries poll_ready until Ready — correct behaviour.
            let buf = tower::ServiceExt::ready(&mut buf)
                .await
                .expect("buffer closed unexpectedly");
            let result = buf.call(0).await;
            assert!(result.is_ok(), "unexpected error: {:?}", result.err());
        }
    }

    /// Confirms `Pending` does appear when the channel is genuinely full, verifying
    /// that the test harness can distinguish real backpressure from the coop-budget artifact.
    #[tokio::test]
    async fn poll_ready_pending_when_channel_full() {
        const BOUND: usize = 2;

        let base: InstrumentedBuffer<u32, _> =
            InstrumentedBuffer::new("test_full", vec![], EchoService, BOUND);

        // Hold all BOUND permits without consuming them.
        let mut holders: Vec<InstrumentedBuffer<u32, _>> =
            (0..BOUND).map(|_| base.clone()).collect();

        for h in &mut holders {
            // Use ready() so the permit is actually acquired (not just one poll).
            tower::ServiceExt::ready(h)
                .await
                .expect("buffer closed unexpectedly");
        }

        // Now a fresh clone should find zero capacity and return Pending.
        let mut starved = base.clone();
        let got_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = got_pending.clone();

        poll_fn(|cx| {
            match starved.poll_ready(cx) {
                Poll::Pending => {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    Poll::Ready(())
                }
                Poll::Ready(_) => Poll::Ready(()),
            }
        })
        .await;

        assert!(
            got_pending.load(std::sync::atomic::Ordering::Relaxed),
            "Expected Pending when all {BOUND} channel slots are held — \
             test harness is broken if this assertion fails"
        );
    }
}
