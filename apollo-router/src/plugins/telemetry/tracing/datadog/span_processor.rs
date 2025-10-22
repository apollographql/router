use opentelemetry::Context;
use opentelemetry::trace::SpanContext;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::Span;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanProcessor;

/// When using the Datadog agent we need spans to always be exported. However, the batch span processor will only export spans that are sampled.
/// This wrapper will override the trace flags to always sample.
/// THe datadog exporter itself will look at the `sampling.priority` trace context attribute to determine if the span should be sampled.
#[derive(Debug)]
pub(crate) struct DatadogSpanProcessor<T: SpanProcessor> {
    delegate: T,
}

impl<T: SpanProcessor> DatadogSpanProcessor<T> {
    pub(crate) fn new(delegate: T) -> Self {
        Self { delegate }
    }
}

impl<T: SpanProcessor> SpanProcessor for DatadogSpanProcessor<T> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.delegate.on_start(span, cx)
    }

    fn on_end(&self, mut span: SpanData) {
        // Note that the trace state for measuring and sampling priority is handled in the AgentSampler
        // The only purpose of this span processor is to ensure that a span can pass through a batch processor.
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

    fn force_flush(&self) -> OTelSdkResult {
        self.delegate.force_flush()
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.delegate.shutdown()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.delegate.set_resource(resource)
    }

    fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> OTelSdkResult {
        self.delegate.shutdown_with_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::SystemTime;

    use opentelemetry::Context;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry_sdk::trace::SpanEvents;
    use opentelemetry_sdk::trace::SpanLinks;
    use opentelemetry_sdk::trace::SpanProcessor;
    use parking_lot::Mutex;

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
            self.spans.lock().push(span);
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: std::time::Duration) -> OTelSdkResult {
            Ok(())
        }
    }

    #[test]
    fn test_on_end_updates_trace_flags() {
        let mock_processor = MockSpanProcessor::new();
        let processor = DatadogSpanProcessor::new(mock_processor.clone());
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
            attributes: Vec::with_capacity(32),
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status: Default::default(),
            instrumentation_scope: Default::default(),
            dropped_attributes_count: 0,
        };

        processor.on_end(span_data.clone());

        // Verify that the trace flags are updated to sampled
        let updated_trace_flags = span_data.span_context.trace_flags().with_sampled(true);
        let stored_spans = mock_processor.spans.lock();
        assert_eq!(stored_spans.len(), 1);
        assert_eq!(
            stored_spans[0].span_context.trace_flags(),
            updated_trace_flags
        );
    }
}
