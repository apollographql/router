//! Instrumentation layer that allows services to be wrapped in a span.
//!
//! See [`Layer`] and [`Service`] for more details.
//!
//! Using ServiceBuilderExt:
//!
//! Now calls to the wrapped service will be wrapped in a span. You can attach attributes to the span from the request.
//!

use std::future::Future;
use std::task::{Context, Poll};
use tower::Layer;
use tower_service::Service;

#[derive(Clone)]
pub struct MapFutureWithContextLayer<C, F> {
    ctx_fn: C,
    map_fn: F,
}

impl<C, F> MapFutureWithContextLayer<C, F> {
    pub fn new(ctx_fn: C, map_fn: F) -> Self {
        Self { ctx_fn, map_fn }
    }
}

impl<S, C, F> Layer<S> for MapFutureWithContextLayer<C, F>
where
    F: Clone,
    C: Clone,
{
    type Service = MapFutureWithContextService<S, C, F>;

    fn layer(&self, inner: S) -> Self::Service {
        MapFutureWithContextService::new(inner, self.ctx_fn.clone(), self.map_fn.clone())
    }
}

pub struct MapFutureWithContextService<S, C, F> {
    inner: S,
    ctx_fn: C,
    map_fn: F,
}

impl<S, C, F> MapFutureWithContextService<S, C, F> {
    pub fn new(inner: S, ctx_fn: C, map_fn: F) -> MapFutureWithContextService<S, C, F>
    where
        C: Clone,
        F: Clone,
    {
        MapFutureWithContextService {
            inner,
            ctx_fn,
            map_fn,
        }
    }
}

impl<R, S, F, C, T, E, Fut, Ctx> Service<R> for MapFutureWithContextService<S, C, F>
where
    S: Service<R>,
    C: FnMut(&R) -> Ctx,
    F: FnMut(Ctx, S::Future) -> Fut,
    E: From<S::Error>,
    Fut: Future<Output = Result<T, E>>,
{
    type Response = T;
    type Error = E;
    type Future = Fut;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(From::from)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let ctx = (self.ctx_fn)(&req);
        (self.map_fn)(ctx, self.inner.call(req))
    }
}
