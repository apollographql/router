use std::borrow::Cow;
use std::sync::Arc;
use std::sync::RwLock;

use opentelemetry::trace::SpanBuilder;
use opentelemetry::trace::Tracer;

use crate::plugins::telemetry::otel::OtelData;
use crate::plugins::telemetry::otel::PreSampledTracer;

#[derive(Clone)]
pub(crate) struct ReloadTracer<S> {
    parent: Arc<RwLock<S>>,
}

impl<S: PreSampledTracer> PreSampledTracer for ReloadTracer<S> {
    fn sampled_context(&self, data: &mut OtelData) -> opentelemetry::Context {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .sampled_context(data)
    }

    fn new_trace_id(&self) -> opentelemetry::trace::TraceId {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .new_trace_id()
    }

    fn new_span_id(&self) -> opentelemetry::trace::SpanId {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .new_span_id()
    }
}

impl<S: Tracer> Tracer for ReloadTracer<S> {
    type Span = S::Span;

    fn start<T>(&self, name: T) -> Self::Span
    where
        T: Into<Cow<'static, str>>,
    {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .start(name)
    }

    fn start_with_context<T>(&self, name: T, parent_cx: &opentelemetry::Context) -> Self::Span
    where
        T: Into<Cow<'static, str>>,
    {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .start_with_context(name, parent_cx)
    }

    fn span_builder<T>(&self, name: T) -> SpanBuilder
    where
        T: Into<Cow<'static, str>>,
    {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .span_builder(name)
    }

    fn build(&self, builder: SpanBuilder) -> Self::Span {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .build(builder)
    }

    fn build_with_context(
        &self,
        builder: SpanBuilder,
        parent_cx: &opentelemetry::Context,
    ) -> Self::Span {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .build_with_context(builder, parent_cx)
    }

    fn in_span<T, F, N>(&self, name: N, f: F) -> T
    where
        F: FnOnce(opentelemetry::Context) -> T,
        N: Into<Cow<'static, str>>,
        Self::Span: Send + Sync + 'static,
    {
        self.parent
            .read()
            .expect("parent tracer must be available")
            .in_span(name, f)
    }
}

impl<S> ReloadTracer<S> {
    pub(crate) fn new(parent: S) -> Self {
        Self {
            parent: Arc::new(RwLock::new(parent)),
        }
    }

    pub(crate) fn reload(&self, new: S) {
        *self
            .parent
            .write()
            .expect("parent tracer must be available") = new;
    }
}
