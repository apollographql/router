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
//
// /// [`Layer`] for service.
// pub struct MapFutureWithContextLayer<ContextFn, MapFn, Ctx, Request, Input, Output, Error>
// where
//     ContextFn: Fn(&Request) -> Ctx,
//     MapFn: (Fn(Ctx, Result<Input, Error>) -> BoxFuture<'static, Result<Output, Error>>) + Clone,
// {
//     context_fn: ContextFn,
//     map_fn: MapFn,
//     phantom: PhantomData<(Request, Input, Output, Error)>,
// }
//
// impl<ContextFn, MapFn, Ctx, Request, Response, Error>
//     MapFutureWithContextLayer<ContextFn, MapFn, Ctx, Request, Input, Output, Error>
// where
//     ContextFn: Fn(&Request) -> Ctx,
//     Fut: Future<Output = Result<Response, Error>>,
//     MapFn: (Fn(Ctx, Fut) -> Fut,
//
// {
//     pub fn new(
//         context_fn: ContextFn,
//         map_fn: MapFn,
//     ) -> MapFutureWithContextLayer<ContextFn, MapFn, Ctx, Request, Input, Output, Error> {
//         Self {
//             context_fn,
//             map_fn,
//             phantom: Default::default(),
//         }
//     }
// }
//
// impl<ContextFn, MapFn, Ctx, Request, Output, S> Layer<S>
//     for MapFutureWithContextLayer<ContextFn, MapFn, Ctx, Request, S::Response, Output, S::Error>
// where
//     S: Service<Request> + Send,
//     <S as Service<Request>>::Future: Send,
//     <S as Service<Request>>::Response: Send,
//     <S as Service<Request>>::Error: Send,
//     ContextFn: (Fn(&Request) -> Ctx) + Clone,
//     MapFn: (Fn(Ctx, Result<S::Response, S::Error>) -> BoxFuture<'static, Result<Output, S::Error>>)
//         + Clone
//         + Send,
// {
//     type Service = MapFutureWithContextService<ContextFn, MapFn, Ctx, Request, Output, S>;
//
//     fn layer(&self, inner: S) -> Self::Service {
//         MapFutureWithContextService {
//             inner,
//             context_fn: self.context_fn.clone(),
//             map_fn: self.map_fn.clone(),
//             phantom: Default::default(),
//         }
//     }
// }
//
// /// [`Service`] for wrapping.
// pub struct MapFutureWithContextService<ContextFn, MapFn, Ctx, Request, Output, S>
// where
//     S: Service<Request> + Send,
//     <S as Service<Request>>::Future: Send,
//     <S as Service<Request>>::Response: Send,
//     <S as Service<Request>>::Error: Send,
//     ContextFn: (Fn(&Request) -> Ctx) + Clone,
//     MapFn: (Fn(Ctx, Result<S::Response, S::Error>) -> BoxFuture<'static, Result<Output, S::Error>>)
//         + Clone
//         + Send,
// {
//     inner: S,
//     context_fn: ContextFn,
//     map_fn: MapFn,
//     phantom: PhantomData<Request>,
// }
//
// impl<ContextFn, MapFn, Ctx, Request, Output, S> Service<Request>
//     for MapFutureWithContextService<ContextFn, MapFn, Ctx, Request, Output, S>
// where
//     S: Service<Request> + Send,
//     <S as Service<Request>>::Future: Send + 'static,
//     <S as Service<Request>>::Response: Send,
//     <S as Service<Request>>::Error: Send + 'static,
//     Ctx: Send + 'static,
//     Output: 'static,
//     ContextFn: (Fn(&Request) -> Ctx) + Clone,
//     Fut: Future<Output = Result<Response, Error>>,
//     MapFn: (Fn(Ctx, Fut) -> BoxFuture<'static, Result<Output, S::Error>>)
//         + Clone
//         + Send
//         + 'static,
// {
//     type Response = Output;
//     type Error = S::Error;
//     type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;
//
//     fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
//         self.inner.poll_ready(cx)
//     }
//
//     fn call(&mut self, req: Request) -> Self::Future {
//         let context = (self.context_fn)(&req);
//         let map_fn = self.map_fn.clone();
//         self.inner
//             .call(req)
//             .then(move |resp| map_fn(context, resp))
//             .boxed()
//     }
// }
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
