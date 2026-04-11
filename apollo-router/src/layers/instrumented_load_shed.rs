//! A wrapper around [`LoadShed`] that increments a counter every time load
//! is shed, reporting it as the `apollo.router.shaping.shed` metric.
//!
//! [`InstrumentedLoadShedLayer`] is a replication of Tower's [`LoadShedLayer`]
//! that produces an [`InstrumentedLoadShed`] service.
//! [`InstrumentedLoadShed`] is a thin wrapper around Tower's [`LoadShed`],
//! and [`InstrumentedResponseFuture`] is a thin wrapper around Tower's
//! [`ResponseFuture`].
//!
//! ## Design trade-offs
//!
//! Similar to how [`UnconstrainedBuffer`] simplifies its instrumentation by
//! counting requests from the moment they enter the queue to the moment they
//! complete (allowing for a `bound + 1` count), [`InstrumentedLoadShed`] also
//! makes compromises in the name of simplicity.
//!
//! In order to avoid re-implementing the [`LoadShed`] service — which could
//! get out of sync with Tower's implementation and future changes —
//! [`InstrumentedResponseFuture`] only counts shedding upon [`Future::poll`].
//! This is because Tower's [`ResponseFuture`] does not expose `ResponseState`,
//! which could otherwise be used to determine whether shedding happened
//! without having to poll the future.
//!
//! The advantage of this model is that only sheds that would be observed by a
//! caller are reported. In other words, requests that were shed but whose
//! futures were dropped without ever being polled do not count toward the
//! shedding metric.
//!
//! [`LoadShedLayer`]: tower::load_shed::LoadShedLayer
//! [`LoadShed`]: LoadShed
//! [`ResponseFuture`]: ResponseFuture
//! [`Future::poll`]: Future::poll
//! [`UnconstrainedBuffer`]: super::unconstrained_buffer::UnconstrainedBuffer

use std::fmt;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use opentelemetry::KeyValue;
use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::load_shed::LoadShed;
use tower::load_shed::error::Overloaded;
use tower::load_shed::future::ResponseFuture;
use tower_service::Service;

/// Adds an instrumented [`LoadShed`] layer to a service.
///
/// See the module documentation for more details.
#[derive(Clone, Debug)]
pub struct InstrumentedLoadShedLayer {
    name: String,
    attributes: Vec<KeyValue>,
}

impl InstrumentedLoadShedLayer {
    /// Creates a new [`InstrumentedLoadShedLayer`] with the provided `name` and `attributes`.
    pub fn new(name: impl Into<String>, attributes: Vec<KeyValue>) -> Self {
        Self {
            name: name.into(),
            attributes,
        }
    }
}

impl<S> Layer<S> for InstrumentedLoadShedLayer {
    type Service = InstrumentedLoadShed<S>;

    fn layer(&self, service: S) -> Self::Service {
        InstrumentedLoadShed::new(self.name.clone(), service, self.attributes.clone())
    }
}

/// A wrapper around [`LoadShed`] that counts
/// shedding events upon [`Future::poll`].
///
/// See the module documentation for more details.
#[derive(Debug)]
pub struct InstrumentedLoadShed<S> {
    inner: LoadShed<S>,
    attributes: Vec<KeyValue>,
}

impl<S> InstrumentedLoadShed<S> {
    fn new(name: impl Into<String>, inner: S, mut attributes: Vec<KeyValue>) -> Self {
        attributes.push(KeyValue::new("layer.service.name", name.into()));
        InstrumentedLoadShed {
            inner: LoadShed::new(inner),
            attributes,
        }
    }
}

impl<S, Req> Service<Req> for InstrumentedLoadShed<S>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = InstrumentedResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        InstrumentedResponseFuture::new(self.inner.call(req), self.attributes.clone())
    }
}

impl<S: Clone> Clone for InstrumentedLoadShed<S> {
    fn clone(&self) -> Self {
        InstrumentedLoadShed {
            inner: self.inner.clone(),
            attributes: self.attributes.clone(),
        }
    }
}

pin_project! {
    /// A wrapper around Tower's [`ResponseFuture`]
    /// that increments the `apollo.router.shaping.shed` counter when shedding
    /// is observed during polling.
    pub struct InstrumentedResponseFuture<F> {
        #[pin]
        inner: ResponseFuture<F>,
        attributes: Vec<KeyValue>,
    }
}

impl<F> InstrumentedResponseFuture<F> {
    fn new(inner: ResponseFuture<F>, attributes: Vec<KeyValue>) -> Self {
        InstrumentedResponseFuture { inner, attributes }
    }
}

impl<F, T, E> Future for InstrumentedResponseFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<BoxError>,
{
    type Output = Result<T, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let attributes = self.attributes.clone();
        match self.project().inner.poll(cx) {
            Poll::Ready(Ok(res)) => Poll::Ready(Ok(res)),
            Poll::Ready(Err(err)) if err.is::<Overloaded>() => {
                u64_counter_with_unit!(
                    "apollo.router.shaping.shed",
                    "Number of times that load was shed",
                    "{shed}",
                    1,
                    attributes
                );
                Poll::Ready(Err(err.into()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<F> fmt::Debug for InstrumentedResponseFuture<F>
where
    // bounds for future-proofing...
    F: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("InstrumentedResponseFuture").finish()
    }
}
