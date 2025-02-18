use std::borrow::Cow;
use std::sync::Arc;

use opentelemetry::trace::SpanBuilder;
use opentelemetry::trace::Tracer;
use parking_lot::RwLock;

use crate::plugins::telemetry::otel::OtelData;
use crate::plugins::telemetry::otel::PreSampledTracer;

#[derive(Clone)]
pub(crate) struct ReloadTracer<S> {
    parent: Arc<RwLock<S>>,
}

impl<S: PreSampledTracer> PreSampledTracer for ReloadTracer<S> {
    fn sampled_context(&self, data: &mut OtelData) -> opentelemetry::Context {
        self.parent.read().sampled_context(data)
    }

    fn new_trace_id(&self) -> opentelemetry::trace::TraceId {
        self.parent.read().new_trace_id()
    }

    fn new_span_id(&self) -> opentelemetry::trace::SpanId {
        self.parent.read().new_span_id()
    }
}

impl<S: Tracer> Tracer for ReloadTracer<S> {
    type Span = S::Span;

    fn start<T>(&self, name: T) -> Self::Span
    where
        T: Into<Cow<'static, str>>,
    {
        self.parent.read().start(name)
    }

    fn start_with_context<T>(&self, name: T, parent_cx: &opentelemetry::Context) -> Self::Span
    where
        T: Into<Cow<'static, str>>,
    {
        self.parent.read().start_with_context(name, parent_cx)
    }

    fn span_builder<T>(&self, name: T) -> SpanBuilder
    where
        T: Into<Cow<'static, str>>,
    {
        self.parent.read().span_builder(name)
    }

    fn build(&self, builder: SpanBuilder) -> Self::Span {
        self.parent.read().build(builder)
    }

    fn build_with_context(
        &self,
        builder: SpanBuilder,
        parent_cx: &opentelemetry::Context,
    ) -> Self::Span {
        self.parent.read().build_with_context(builder, parent_cx)
    }

    fn in_span<T, F, N>(&self, name: N, f: F) -> T
    where
        F: FnOnce(opentelemetry::Context) -> T,
        N: Into<Cow<'static, str>>,
        Self::Span: Send + Sync + 'static,
    {
        self.parent.read().in_span(name, f)
    }
}

impl<S> ReloadTracer<S> {
    pub(crate) fn new(parent: S) -> Self {
        Self {
            parent: Arc::new(RwLock::new(parent)),
        }
    }

    pub(crate) fn reload(&self, new: S) {
        *self.parent.write() = new;
    }
}
