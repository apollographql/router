use opentelemetry_api::trace::SpanContext;
use opentelemetry_api::trace::TraceResult;
use opentelemetry_api::Context;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::trace::Span;
use opentelemetry_sdk::trace::SpanProcessor;

/// When using the Datadog agent we need spans to always be exported. However, the batch span processor will only export spans that are sampled.
/// This wrapper will override the trace flags to always sample.
/// THe datadog exporter itself will look at the `sampling.priority` trace context attribute to determine if the span should be sampled.
#[derive(Debug)]
pub(crate) struct BatchSpanProcessor<T: SpanProcessor> {
    delegate: T,
}

impl<T: SpanProcessor> BatchSpanProcessor<T> {
    pub(crate) fn new(delegate: T) -> Self {
        Self { delegate }
    }
}

impl<T: SpanProcessor> SpanProcessor for BatchSpanProcessor<T> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.delegate.on_start(span, cx)
    }

    fn on_end(&self, mut span: SpanData) {
        let new_trace_flags = span.span_context.trace_flags().with_sampled(true);
        span.span_context = SpanContext::new(
            span.span_context.trace_id(),
            span.span_context.span_id(),
            new_trace_flags,
            span.span_context.is_remote(),
            span.span_context.trace_state().clone(),
        );
        self.delegate.on_end(span)
    }

    fn force_flush(&self) -> TraceResult<()> {
        self.delegate.force_flush()
    }

    fn shutdown(&mut self) -> TraceResult<()> {
        self.delegate.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::SystemTime;

    use opentelemetry_api::trace::SpanId;
    use opentelemetry_api::trace::SpanKind;
    use opentelemetry_api::trace::TraceFlags;
    use opentelemetry_api::trace::TraceId;
    use opentelemetry_api::Context;
    use opentelemetry_sdk::trace::EvictedHashMap;
    use opentelemetry_sdk::trace::EvictedQueue;
    use opentelemetry_sdk::trace::SpanProcessor;

    use super::*;

    #[derive(Debug, Clone)]
    struct MockSpanProcessor {
        spans: Arc<Mutex<Vec<SpanData>>>,
    }

    impl MockSpanProcessor {
        fn new() -> Self {
            Self {
                spans: Default::default(),
            }
        }
    }

    impl SpanProcessor for MockSpanProcessor {
        fn on_start(&self, _span: &mut Span, _cx: &Context) {}

        fn on_end(&self, span: SpanData) {
            self.spans.lock().unwrap().push(span);
        }

        fn force_flush(&self) -> TraceResult<()> {
            Ok(())
        }

        fn shutdown(&mut self) -> TraceResult<()> {
            Ok(())
        }
    }

    #[test]
    fn test_on_end_updates_trace_flags() {
        let mock_processor = MockSpanProcessor::new();
        let processor = BatchSpanProcessor::new(mock_processor.clone());
        let span_context = SpanContext::new(
            TraceId::from_u128(1),
            SpanId::from_u64(1),
            TraceFlags::default(),
            false,
            Default::default(),
        );
        let span_data = SpanData {
            span_context,
            parent_span_id: SpanId::from_u64(1),
            span_kind: SpanKind::Client,
            name: Default::default(),
            start_time: SystemTime::now(),
            end_time: SystemTime::now(),
            attributes: EvictedHashMap::new(32, 32),
            events: EvictedQueue::new(32),
            links: EvictedQueue::new(32),
            status: Default::default(),
            resource: Default::default(),
            instrumentation_lib: Default::default(),
        };

        processor.on_end(span_data.clone());

        // Verify that the trace flags are updated to sampled
        let updated_trace_flags = span_data.span_context.trace_flags().with_sampled(true);
        let stored_spans = mock_processor.spans.lock().unwrap();
        assert_eq!(stored_spans.len(), 1);
        assert_eq!(
            stored_spans[0].span_context.trace_flags(),
            updated_trace_flags
        );
    }
}
