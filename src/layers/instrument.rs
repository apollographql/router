use std::marker::PhantomData;
use std::task::Poll;

use tower::{Layer, Service};
use tracing::instrument::{Instrument as OtherInstrument, Instrumented};
use tracing::Span;

#[derive(Debug, Clone)]
pub struct InstrumentLayer<Request, FnType>
where
    FnType: Fn(&Request) -> Span,
{
    fn_span: FnType,
    phantom: PhantomData<Request>,
}

impl<Request, FnType> InstrumentLayer<Request, FnType>
where
    FnType: Fn(&Request) -> Span,
{
    pub(crate) fn new(fn_span: FnType) -> InstrumentLayer<Request, FnType> {
        Self {
            fn_span,
            phantom: PhantomData::default(),
        }
    }
}

pub struct Instrument<S, Request, FnType>
where
    S: Service<Request>,
    FnType: Fn(&Request) -> Span,
{
    service: S,
    fn_span: FnType,
    phantom: PhantomData<Request>,
}

impl<S, Request, FnType> Service<Request> for Instrument<S, Request, FnType>
where
    S: Service<Request>,
    FnType: Fn(&Request) -> Span,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Instrumented<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let span = (self.fn_span)(&req);
        self.service.call(req).instrument(span)
    }
}

impl<S, Request, FnType> Layer<S> for InstrumentLayer<Request, FnType>
where
    S: Service<Request>,
    FnType: Fn(&Request) -> Span + Clone,
{
    type Service = Instrument<S, Request, FnType>;

    fn layer(&self, service: S) -> Self::Service {
        Instrument {
            service,
            fn_span: self.fn_span.clone(),
            phantom: Default::default(),
        }
    }
}
