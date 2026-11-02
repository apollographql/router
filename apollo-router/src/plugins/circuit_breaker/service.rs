//! The service a subgraph or connector source sits behind while circuit breaking is on.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::ready;

use apollo_qos::circuit_breaker::CircuitBreakerLayer;
use apollo_qos::circuit_breaker::CircuitBreakerService;
use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;
use tower::util::Oneshot;

use super::Classifier;

/// What the circuit breaker needs to know about one kind of target's requests.
pub(crate) trait Target: Clone {
    type Request: 'static;
    type Response;

    /// Whether `request` goes around the target's circuit in both directions: it is neither
    /// turned away by an open circuit nor recorded.
    fn bypasses_circuit(request: &Self::Request) -> bool;

    /// The answer for a request the open circuit turned away.
    fn reject(&self, request: Self::Request) -> Self::Response;
}

/// Puts a target's service behind that target's circuit.
///
/// Built by [`CircuitBreaker`](super::CircuitBreaker) for one subgraph or connector source. Every
/// layer built for a target shares that target's circuit, however many services it wraps.
pub(crate) struct CircuitLayer<T: Target> {
    circuit: CircuitBreakerLayer<Classifier<T::Response>>,
    target: T,
}

impl<T: Target> CircuitLayer<T> {
    pub(super) fn new(circuit: CircuitBreakerLayer<Classifier<T::Response>>, target: T) -> Self {
        Self { circuit, target }
    }
}

impl<T: Target, S> Layer<S> for CircuitLayer<T> {
    type Service = Protected<T, S>;

    fn layer(&self, service: S) -> Self::Service {
        Protected::new(self.circuit.clone(), service, self.target.clone())
    }
}

/// A target's service, behind that target's circuit.
///
/// Wraps the apollo-qos [`CircuitBreakerService`] with what the router needs of it that it does
/// not do on its own:
///
/// - A request the circuit rejects is answered with the target's own kind of error response,
///   built from the request itself. apollo-qos drops a request it rejects inside `call`, so
///   [`Admission`] hands it back from its `Drop` rather than the rejection being built from
///   copies taken off every request on the chance that it is rejected.
/// - A request the circuit must not judge goes around it, so an open circuit does not turn it
///   away: a mapping-only connector request, which never reaches the target, and a batched
///   subgraph fetch, which the rest of its batch waits for.
/// - An open circuit fails fast: `poll_ready` asks the circuit first, and readies the target's
///   service only when the circuit would let a request through, as apollo-qos's own service
///   does. A request the circuit will turn away doesn't wait behind a target's service that is
///   not ready, such as a concurrency limit a plugin puts under the circuit.
/// - The readiness `poll_ready` obtains from the target's service goes with the request
///   whichever way `call` sends it: to the target through the circuit, around the circuit, or
///   nowhere at all. None is left reserved on a service the request did not use, which a
///   service that hands out a permit per `poll_ready` would otherwise keep until the next call.
pub(crate) struct Protected<T: Target, S> {
    /// The target's circuit, to build a breaker for each clone, so that no two clones share an
    /// [`Admit`]'s record of the circuit's answer.
    circuit: CircuitBreakerLayer<Classifier<T::Response>>,
    breaker: CircuitBreakerService<Classifier<T::Response>, Admit>,
    /// Whether the circuit would let the next request through, as its last `poll_ready` told
    /// [`Admit`].
    admitting: Arc<AtomicBool>,
    /// The target's service. Readied by `poll_ready` when the circuit would let the request
    /// through, and handed on by `call` with the request, leaving a clone for the next one.
    inner: S,
    target: T,
}

impl<T: Target, S> Protected<T, S> {
    fn new(circuit: CircuitBreakerLayer<Classifier<T::Response>>, inner: S, target: T) -> Self {
        let admitting = Arc::new(AtomicBool::new(false));
        Self {
            breaker: circuit.layer(Admit {
                admitting: admitting.clone(),
            }),
            circuit,
            admitting,
            inner,
            target,
        }
    }
}

// Written out because a derive would also require `T::Response: Clone`, which a subgraph
// response is not: only the classifier's function pointer mentions it.
impl<T: Target, S: Clone> Clone for Protected<T, S> {
    fn clone(&self) -> Self {
        Self::new(
            self.circuit.clone(),
            self.inner.clone(),
            self.target.clone(),
        )
    }
}

impl<T, S> Service<T::Request> for Protected<T, S>
where
    T: Target,
    S: Service<T::Request, Response = T::Response, Error = BoxError> + Clone,
{
    type Response = T::Response;
    type Error = BoxError;
    type Future = ProtectedFuture<
        <CircuitBreakerService<Classifier<T::Response>, Admit> as Service<
            Admission<'static, T::Request, S>,
        >>::Future,
        Oneshot<S, T::Request>,
        T::Response,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The circuit polls `Admit` only when it would let a request through, and `Admit` has
        // nothing to wait for, so this learns the circuit's answer without waiting.
        self.admitting.store(false, Ordering::Relaxed);
        ready!(Service::<Admission<'_, T::Request, S>>::poll_ready(
            &mut self.breaker,
            cx
        ))?;
        if self.admitting.load(Ordering::Relaxed) {
            ready!(self.inner.poll_ready(cx))?;
        }
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: T::Request) -> Self::Future {
        let clone = self.inner.clone();
        let service = std::mem::replace(&mut self.inner, clone);

        // A request that goes around the circuit needs the target's service whatever the circuit
        // said, so it waits for readiness here if `poll_ready` didn't obtain it.
        if T::bypasses_circuit(&request) {
            return ProtectedFuture::Bypassed {
                future: service.oneshot(request),
            };
        }

        let returned = Cell::new(None);
        let future = self.breaker.call(Admission {
            request: Some(request),
            service,
            returned: &returned,
        });
        match returned.into_inner() {
            None => ProtectedFuture::Admitted { future },
            Some(request) => ProtectedFuture::Rejected {
                future,
                rejection: Some(self.target.reject(request)),
            },
        }
    }
}

/// Carries a request through the circuit, and hands it back if the circuit turns it away.
///
/// Borrows the slot it hands the request back to, so the request cannot outlive the call to the
/// circuit: apollo-qos either passes it on to [`Admit`] or drops it before `call` returns.
pub(crate) struct Admission<'a, Req, S> {
    request: Option<Req>,
    /// The target's service, readied for this request when the circuit said it would let it
    /// through. Dropped with the admission when the circuit turns the request away, which gives
    /// back whatever its readiness reserved.
    service: S,
    returned: &'a Cell<Option<Req>>,
}

impl<Req, S> Drop for Admission<'_, Req, S> {
    fn drop(&mut self) {
        // Only still holding the request when the circuit rejected it: an admitted request is
        // taken out by `Admit` on its way to the target.
        if let Some(request) = self.request.take() {
            self.returned.set(Some(request));
        }
    }
}

/// The target's service as the circuit sees it: calls the service each admission carries with the
/// request it carries.
///
/// Holds no service of its own, so it is always ready: the one an admission carries was readied
/// by [`Protected`] before the request reached the circuit. The circuit asks for its readiness
/// only when it would let the next request through, and it records that for [`Protected`].
pub(crate) struct Admit {
    admitting: Arc<AtomicBool>,
}

impl<'a, Req, S> Service<Admission<'a, Req, S>> for Admit
where
    S: Service<Req, Error = BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = S::Future;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.admitting.store(true, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut admission: Admission<'a, Req, S>) -> Self::Future {
        let request = admission
            .request
            .take()
            .expect("a request is admitted only once");
        admission.service.call(request)
    }
}

pin_project! {
    /// Future returned by [`Protected`].
    #[project = ProtectedFutureProj]
    pub(crate) enum ProtectedFuture<F, B, Res> {
        /// Admitted by the circuit, and on its way to the target.
        Admitted { #[pin] future: F },
        /// Turned away by the circuit, with the answer already built.
        Rejected { #[pin] future: F, rejection: Option<Res> },
        /// Sent around the circuit.
        Bypassed { #[pin] future: B },
    }
}

impl<F, B, Res> Future for ProtectedFuture<F, B, Res>
where
    F: Future<Output = Result<Res, BoxError>>,
    B: Future<Output = Result<Res, BoxError>>,
{
    type Output = Result<Res, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            ProtectedFutureProj::Admitted { future } => future.poll(cx),
            ProtectedFutureProj::Rejected { future, rejection } => {
                // Only the circuit's error comes out of this, and the rejection stands in for it.
                // It is still polled, because its first poll is what opens the request's span and
                // records any state change on it.
                let _ = ready!(future.poll(cx));
                Poll::Ready(Ok(rejection
                    .take()
                    .expect("a rejection is returned only once")))
            }
            ProtectedFutureProj::Bypassed { future } => future.poll(cx),
        }
    }
}
