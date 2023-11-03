//! Instrumentation layer that allows services to be wrapped in a span.
//!
//! Using ServiceBuilderExt:
//! ```rust
//! # use tower::ServiceBuilder;
//! # use tower_service::Service;
//! # use tracing::info_span;
//! # use apollo_router::layers::ServiceBuilderExt;
//! # fn test<T>(service: impl Service<T>) {
//! let instrumented = ServiceBuilder::new()
//!             .instrument(|_request| info_span!("query_planning"))
//!             .service(service);
//! # }
//! ```
//! Now calls to the wrapped service will be wrapped in a span. You can attach attributes to the span from the request.
//!
//! See [`Layer`] and [`Service`] for more details.

use std::marker::PhantomData;
use std::task::Context;
use std::task::Poll;

use tower::Layer;
use tower_service::Service;
use tracing::Instrument;

/// [`Layer`] for instrumentation. See [`ServiceBuilderExt::instrument()`](crate::layers::ServiceBuilderExt::instrument()).
pub struct InstrumentLayer<F, Request>
where
    F: Fn(&Request) -> tracing::Span,
{
    span_fn: F,
    phantom: PhantomData<Request>,
}

impl<F, Request> InstrumentLayer<F, Request>
where
    F: Fn(&Request) -> tracing::Span,
{
    #[allow(missing_docs)] // FIXME
    pub fn new(span_fn: F) -> InstrumentLayer<F, Request> {
        Self {
            span_fn,
            phantom: Default::default(),
        }
    }
}

impl<F, S, Request> Layer<S> for InstrumentLayer<F, Request>
where
    S: Service<Request>,
    F: Fn(&Request) -> tracing::Span + Clone,
{
    type Service = InstrumentService<F, S, Request>;

    fn layer(&self, inner: S) -> Self::Service {
        InstrumentService {
            inner,
            span_fn: self.span_fn.clone(),
            phantom: Default::default(),
        }
    }
}

/// [`Service`] for instrumentation. See [`ServiceBuilderExt::instrument()`](crate::layers::ServiceBuilderExt::instrument()).
pub struct InstrumentService<F, S, Request>
where
    S: Service<Request>,
    F: Fn(&Request) -> tracing::Span,
{
    inner: S,
    span_fn: F,
    phantom: PhantomData<Request>,
}

impl<F, S, Request> Service<Request> for InstrumentService<F, S, Request>
where
    F: Fn(&Request) -> tracing::Span,
    S: Service<Request>,
    <S as Service<Request>>::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = tracing::instrument::Instrumented<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let span = (self.span_fn)(&req);
        self.inner.call(req).instrument(span)
    }
}

// // Optional InstrumentLayer

// /// [`Layer`] for instrumentation. See [`ServiceBuilderExt::option_instrument()`](crate::layers::ServiceBuilderExt::option_instrument()).
// pub struct OptionInstrumentLayer<F, Request>
// where
//     F: Fn(&Request) -> Option<tracing::Span>,
// {
//     span_fn: F,
//     phantom: PhantomData<Request>,
// }

// impl<F, Request> OptionInstrumentLayer<F, Request>
// where
//     F: Fn(&Request) -> Option<tracing::Span>,
// {
//     #[allow(missing_docs)] // FIXME
//     pub fn new(span_fn: F) -> OptionInstrumentLayer<F, Request> {
//         Self {
//             span_fn,
//             phantom: Default::default(),
//         }
//     }
// }

// impl<F, S, Request> Layer<S> for OptionInstrumentLayer<F, Request>
// where
//     S: Service<Request>,
//     F: Fn(&Request) -> Option<tracing::Span> + Clone,
// {
//     type Service = OptionInstrumentService<F, S, Request>;

//     fn layer(&self, inner: S) -> Self::Service {
//         OptionInstrumentService {
//             inner,
//             span_fn: self.span_fn.clone(),
//             phantom: Default::default(),
//         }
//     }
// }

// /// [`Service`] for instrumentation. See [`ServiceBuilderExt::option_instrument()`](crate::layers::ServiceBuilderExt::option_instrument()).
// pub struct OptionInstrumentService<F, S, Request>
// where
//     S: Service<Request>,
//     F: Fn(&Request) -> Option<tracing::Span>,
// {
//     inner: S,
//     span_fn: F,
//     phantom: PhantomData<Request>,
// }

// impl<F, S, Request> Service<Request> for OptionInstrumentService<F, S, Request>
// where
//     F: Fn(&Request) -> Option<tracing::Span>,
//     S: Service<Request>,
//     <S as Service<Request>>::Future: Send + 'static,
//     <S as Service<Request>>::Error: std::error::Error + Send + Sync + 'static,
// {
//     type Response = S::Response;
//     type Error = BoxError;
//     type Future = Either<S::Future, tracing::instrument::Instrumented<S::Future>>;

//     fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
//         self.inner.poll_ready(cx).map_err(BoxError::from)
//     }

//     fn call(&mut self, req: Request) -> Self::Future {
//         let span = (self.span_fn)(&req);
//         match span {
//             None => Either::A(self.inner.call(req)),
//             Some(span) => Either::B(self.inner.call(req).instrument(span)),
//         }
//     }
// }
