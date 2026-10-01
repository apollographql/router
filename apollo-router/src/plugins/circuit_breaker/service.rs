//! The service a subgraph or connector source sits behind while circuit breaking is on.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::task::ready;

use apollo_qos::circuit_breaker::CircuitBreakerLayer;
use apollo_qos::circuit_breaker::CircuitBreakerService;
use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::Classifier;

/// What the circuit breaker needs to know about one kind of target's requests.
pub(crate) trait Target: Clone {
    type Request: 'static;
    type Response;

    /// Whether `request` is answered without reaching the target, and so has no business with
    /// the target's circuit in either direction.
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
        Protected {
            breaker: self.circuit.layer(Admit),
            inner: service,
            target: self.target.clone(),
        }
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
/// - A request that never reaches the target, such as a mapping-only connector request, goes
///   around the circuit, so an open circuit does not turn it away.
/// - The readiness `poll_ready` obtains from the target's service goes with the request
///   whichever way `call` sends it: to the target through the circuit, around the circuit, or
///   nowhere at all. None is left reserved on a service the request did not use, which a
///   service that hands out a permit per `poll_ready`, such as a concurrency limit a plugin puts
///   under the circuit, would otherwise keep until the next call.
pub(crate) struct Protected<T: Target, S> {
    breaker: CircuitBreakerService<Classifier<T::Response>, Admit>,
    /// The target's service. Readied by `poll_ready`, and handed on by `call` with the request
    /// it was readied for, leaving a clone to be readied for the next one.
    inner: S,
    target: T,
}

// Written out because a derive would also require `T::Response: Clone`, which a subgraph
// response is not: only the classifier's function pointer mentions it.
impl<T: Target, S: Clone> Clone for Protected<T, S> {
    fn clone(&self) -> Self {
        Self {
            breaker: self.breaker.clone(),
            inner: self.inner.clone(),
            target: self.target.clone(),
        }
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
        S::Future,
        T::Response,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The target's service is readied even while the circuit is open: `poll_ready` cannot
        // know whether the next request goes through the circuit or around it, and one that goes
        // around it needs the service either way. A request the circuit turns away gives the
        // readiness back unused.
        ready!(self.inner.poll_ready(cx))?;
        Service::<Admission<'_, T::Request, S>>::poll_ready(&mut self.breaker, cx)
    }

    fn call(&mut self, request: T::Request) -> Self::Future {
        let clone = self.inner.clone();
        let mut service = std::mem::replace(&mut self.inner, clone);

        if T::bypasses_circuit(&request) {
            return ProtectedFuture::Bypassed {
                future: service.call(request),
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
    /// The target's service, already readied for this request. Dropped with the admission when
    /// the circuit turns the request away, which gives back whatever its readiness reserved.
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
/// by [`Protected`] before the request reached the circuit.
#[derive(Clone, Copy)]
pub(crate) struct Admit;

impl<'a, Req, S> Service<Admission<'a, Req, S>> for Admit
where
    S: Service<Req, Error = BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = S::Future;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
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
