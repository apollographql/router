use std::fmt;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use futures::ready;
use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::load_shed::error::Overloaded;
use tower_service::Service;

/// A [`Layer`] to wrap services in [`LoadShedService`] middleware.
///
/// [`Layer`]: crate::Layer
#[derive(Clone)]
pub struct InstrumentedLoadShedLayer {
    name: String,
    extra: Vec<(String, String)>,
}

impl InstrumentedLoadShedLayer {
    pub fn new(name: impl Into<String>, extra: Vec<(String, String)>) -> Self {
        Self {
            name: name.into(),
            extra,
        }
    }
}

impl<S> Layer<S> for InstrumentedLoadShedLayer {
    type Service = LoadShed<S>;

    fn layer(&self, service: S) -> Self::Service {
        LoadShed::new(attrs(self.name.clone(), self.extra.clone()), service)
    }
}

impl fmt::Debug for InstrumentedLoadShedLayer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("LoadShedLayer").finish()
    }
}

// Service

/// A [`Service`] that sheds load when the inner service isn't ready.
///
/// [`Service`]: crate::Service
#[derive(Debug)]
pub struct LoadShed<S> {
    attrs: Vec<opentelemetry::KeyValue>,
    inner: S,
    is_ready: bool,
}

// ===== impl LoadShed =====

impl<S> LoadShed<S> {
    /// Wraps a service in [`LoadShed`] middleware.
    fn new(attrs: Vec<opentelemetry::KeyValue>, inner: S) -> Self {
        LoadShed {
            attrs,
            inner,
            is_ready: false,
        }
    }
}

impl<S, Req> Service<Req> for LoadShed<S>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.is_ready = match self.inner.poll_ready(cx) {
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
            r => r.is_ready(),
        };

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Req) -> Self::Future {
        if self.is_ready {
            // readiness only counts once, you need to check again!
            self.is_ready = false;
            ResponseFuture::called(self.inner.call(req))
        } else {
            u64_counter_with_unit!(
                "apollo.router.shaping.shed",
                "Number of times that load was shed",
                "{call}",
                1,
                self.attrs
            );
            ResponseFuture::overloaded()
        }
    }
}

impl<S: Clone> Clone for LoadShed<S> {
    fn clone(&self) -> Self {
        LoadShed {
            attrs: self.attrs.clone(),
            inner: self.inner.clone(),
            // new clones shouldn't carry the readiness state, as a cloneable
            // inner service likely tracks readiness per clone.
            is_ready: false,
        }
    }
}

// Future

pin_project! {
    /// Future for the [`LoadShed`] service.
    ///
    /// [`LoadShed`]: crate::load_shed::LoadShed
    pub struct ResponseFuture<F> {
        #[pin]
        state: ResponseState<F>,
    }
}

pin_project! {
    #[project = ResponseStateProj]
    enum ResponseState<F> {
        Called {
            #[pin]
            fut: F
        },
        Overloaded,
    }
}

impl<F> ResponseFuture<F> {
    pub(crate) fn called(fut: F) -> Self {
        ResponseFuture {
            state: ResponseState::Called { fut },
        }
    }

    pub(crate) fn overloaded() -> Self {
        ResponseFuture {
            state: ResponseState::Overloaded,
        }
    }
}

impl<F, T, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<BoxError>,
{
    type Output = Result<T, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().state.project() {
            ResponseStateProj::Called { fut } => {
                Poll::Ready(ready!(fut.poll(cx)).map_err(Into::into))
            }
            ResponseStateProj::Overloaded => Poll::Ready(Err(Overloaded::new().into())),
        }
    }
}

impl<F> fmt::Debug for ResponseFuture<F>
where
    // bounds for future-proofing...
    F: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}

fn attrs(name: String, extra: Vec<(String, String)>) -> Vec<opentelemetry::KeyValue> {
    std::iter::once(opentelemetry::KeyValue::new("shed.name", name))
        .chain(
            extra
                .into_iter()
                .map(|(k, v)| opentelemetry::KeyValue::new(k, v)),
        )
        .collect()
}
