use std::fmt::Debug;
use std::fmt::Formatter;

use futures::future::BoxFuture;
use opentelemetry::KeyValue;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanKind;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::export::trace::ExportResult;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::export::trace::SpanExporter;

use crate::plugins::telemetry::consts::OTEL_ORIGINAL_NAME;
use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;

pub(crate) struct MeasuringExporter<T: SpanExporter> {
    pub(crate) delegate: T,
    pub(crate) span_metrics: ahash::HashMap<String, bool>,
}

impl<T: SpanExporter> Debug for MeasuringExporter<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.delegate.fmt(f)
    }
}

impl<T: SpanExporter> SpanExporter for MeasuringExporter<T> {
    fn export(&mut self, mut batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        // Here we do some special processing of the spans before passing them to the delegate
        // In particular we default the span.kind to the span kind, and also override the trace measure status if we need to.
        for span in &mut batch {
            // If the span metrics are enabled for this span, set the trace state to measuring.
            // We do all this dancing to avoid allocating.
            let original_span_name = span
                .attributes
                .iter()
                .find(|kv| kv.key.as_str() == OTEL_ORIGINAL_NAME)
                .map(|kv| kv.value.as_str());
            let final_span_name = if let Some(span_name) = &original_span_name {
                span_name.as_ref()
            } else {
                span.name.as_ref()
            };

            // If enabled via config
            let metrics_configured = self
                .span_metrics
                .get(final_span_name)
                .copied()
                .unwrap_or_default();

            if metrics_configured {
                // `.with_measuring(true)` eventually causes `_dd.measured` to be set deep inside the
                // datadog exporter. But we also support OTLP export to the datadog agent, and in that
                // case we have to set the attribute manually.
                let new_trace_state = span.span_context.trace_state().with_measuring(true);
                span.span_context = SpanContext::new(
                    span.span_context.trace_id(),
                    span.span_context.span_id(),
                    span.span_context.trace_flags(),
                    span.span_context.is_remote(),
                    new_trace_state,
                );
                // Not necessary for datadog exporter, but doesn't hurt.
                span.attributes.push(KeyValue::new("_dd.measured", "1"));
            }

            // Set the span kind https://github.com/DataDog/dd-trace-go/blob/main/ddtrace/ext/span_kind.go
            let span_kind = match &span.span_kind {
                SpanKind::Client => "client",
                SpanKind::Server => "server",
                SpanKind::Producer => "producer",
                SpanKind::Consumer => "consumer",
                SpanKind::Internal => "internal",
            };
            span.attributes.push(KeyValue::new("span.kind", span_kind));

            // Note we do NOT set span.type as it isn't a good fit for otel.
        }
        self.delegate.export(batch)
    }
    fn shutdown(&mut self) {
        self.delegate.shutdown()
    }
    fn force_flush(&mut self) -> BoxFuture<'static, ExportResult> {
        self.delegate.force_flush()
    }
    fn set_resource(&mut self, resource: &Resource) {
        self.delegate.set_resource(resource);
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::time::SystemTime;

    use ahash::HashMap;
    use ahash::HashMapExt;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::Status;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry_sdk::export::trace::SpanData;
    use opentelemetry_sdk::trace::SpanEvents;
    use opentelemetry_sdk::trace::SpanLinks;

    use super::*;

    // Mock exporter for testing
    #[derive(Debug)]
    struct MockExporter {
        exported_spans: Vec<SpanData>,
    }

    impl MockExporter {
        fn new() -> Self {
            Self {
                exported_spans: Vec::new(),
            }
        }
    }

    impl SpanExporter for MockExporter {
        fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
            self.exported_spans.extend(batch);
            Box::pin(async { Ok(()) })
        }

        fn shutdown(&mut self) {}
        fn force_flush(&mut self) -> BoxFuture<'static, ExportResult> {
            Box::pin(async { Ok(()) })
        }
        fn set_resource(&mut self, _resource: &Resource) {}
    }

    fn create_test_span(name: &str, attributes: Vec<KeyValue>) -> SpanData {
        SpanData {
            span_context: SpanContext::new(
                TraceId::from_u128(1),
                SpanId::from_u64(1),
                TraceFlags::default(),
                false,
                TraceState::default(),
            ),
            parent_span_id: SpanId::from_u64(0),
            span_kind: SpanKind::Internal,
            name: Cow::Owned(name.to_string()),
            start_time: SystemTime::now(),
            end_time: SystemTime::now(),
            attributes,
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status: Status::default(),
            dropped_attributes_count: 0,
            instrumentation_lib: opentelemetry_sdk::InstrumentationLibrary::builder("test")
                .with_version("1.0.0")
                .build(),
        }
    }

    #[tokio::test]
    async fn test_measuring_exporter_adds_measured_attribute_when_enabled() {
        let mut span_metrics = HashMap::new();
        span_metrics.insert("test_span".to_string(), true);

        let mock_exporter = MockExporter::new();
        let mut measuring_exporter = MeasuringExporter {
            delegate: mock_exporter,
            span_metrics,
        };

        let span = create_test_span("test_span", vec![]);

        measuring_exporter.export(vec![span]).await.unwrap();

        // Check that the span was processed and has the measured attribute
        let exported_spans = &measuring_exporter.delegate.exported_spans;
        assert_eq!(exported_spans.len(), 1);

        let processed_span = &exported_spans[0];
        let measured_attr = processed_span
            .attributes
            .iter()
            .find(|attr| attr.key.as_str() == "_dd.measured")
            .expect("_dd.measured attribute should be present");

        assert_eq!(measured_attr.value.as_str(), "1");
    }

    #[tokio::test]
    async fn test_measuring_exporter_skips_measurement_when_disabled() {
        let mut span_metrics = HashMap::new();
        span_metrics.insert("test_span".to_string(), false);

        let mock_exporter = MockExporter::new();
        let mut measuring_exporter = MeasuringExporter {
            delegate: mock_exporter,
            span_metrics,
        };

        let span = create_test_span("test_span", vec![]);

        measuring_exporter.export(vec![span]).await.unwrap();

        // Check that the span was processed but does not have the measured attribute
        let exported_spans = &measuring_exporter.delegate.exported_spans;
        assert_eq!(exported_spans.len(), 1);

        let processed_span = &exported_spans[0];
        let measured_attr = processed_span
            .attributes
            .iter()
            .find(|attr| attr.key.as_str() == "_dd.measured");

        assert!(
            measured_attr.is_none(),
            "_dd.measured attribute should not be present when disabled"
        );
    }

    #[tokio::test]
    async fn test_measuring_exporter_uses_original_name_when_present() {
        let mut span_metrics = HashMap::new();
        span_metrics.insert("original_span".to_string(), true);

        let mock_exporter = MockExporter::new();
        let mut measuring_exporter = MeasuringExporter {
            delegate: mock_exporter,
            span_metrics,
        };

        let span = create_test_span(
            "renamed_span",
            vec![KeyValue::new(OTEL_ORIGINAL_NAME, "original_span")],
        );

        measuring_exporter.export(vec![span]).await.unwrap();

        // Check that the span uses the original name for measurement decision
        let exported_spans = &measuring_exporter.delegate.exported_spans;
        assert_eq!(exported_spans.len(), 1);

        let processed_span = &exported_spans[0];
        let measured_attr = processed_span
            .attributes
            .iter()
            .find(|attr| attr.key.as_str() == "_dd.measured");

        assert!(
            measured_attr.is_some(),
            "_dd.measured attribute should be present based on original name"
        );
    }

    #[tokio::test]
    async fn test_measuring_exporter_adds_span_kind_attribute() {
        let mut span_metrics = HashMap::new();
        span_metrics.insert("test_span".to_string(), true);

        let mock_exporter = MockExporter::new();
        let mut measuring_exporter = MeasuringExporter {
            delegate: mock_exporter,
            span_metrics,
        };

        let mut span = create_test_span("test_span", vec![]);
        span.span_kind = SpanKind::Server;

        measuring_exporter.export(vec![span]).await.unwrap();

        // Check that span.kind attribute was added
        let exported_spans = &measuring_exporter.delegate.exported_spans;
        let processed_span = &exported_spans[0];
        let span_kind_attr = processed_span
            .attributes
            .iter()
            .find(|attr| attr.key.as_str() == "span.kind")
            .expect("span.kind attribute should be present");

        assert_eq!(span_kind_attr.value.as_str(), "server");
    }

    #[tokio::test]
    async fn test_measuring_exporter_defaults_to_not_measured() {
        let span_metrics = HashMap::new(); // Empty - no configuration

        let mock_exporter = MockExporter::new();
        let mut measuring_exporter = MeasuringExporter {
            delegate: mock_exporter,
            span_metrics,
        };

        let span = create_test_span("unknown_span", vec![]);

        measuring_exporter.export(vec![span]).await.unwrap();

        // Check that the span was processed but not measured
        let exported_spans = &measuring_exporter.delegate.exported_spans;
        let processed_span = &exported_spans[0];
        let measured_attr = processed_span
            .attributes
            .iter()
            .find(|attr| attr.key.as_str() == "_dd.measured");

        assert!(
            measured_attr.is_none(),
            "_dd.measured attribute should not be present by default"
        );
    }

    #[tokio::test]
    async fn test_measuring_exporter_processes_multiple_spans() {
        let mut span_metrics = HashMap::new();
        span_metrics.insert("measured_span".to_string(), true);
        span_metrics.insert("unmeasured_span".to_string(), false);

        let mock_exporter = MockExporter::new();
        let mut measuring_exporter = MeasuringExporter {
            delegate: mock_exporter,
            span_metrics,
        };

        let spans = vec![
            create_test_span("measured_span", vec![]),
            create_test_span("unmeasured_span", vec![]),
            create_test_span("unknown_span", vec![]),
        ];

        measuring_exporter.export(spans).await.unwrap();

        // Check processing of all spans
        let exported_spans = &measuring_exporter.delegate.exported_spans;
        assert_eq!(exported_spans.len(), 3);

        // First span should be measured
        let measured_count = exported_spans
            .iter()
            .filter(|span| {
                span.attributes
                    .iter()
                    .any(|attr| attr.key.as_str() == "_dd.measured" && attr.value.as_str() == "1")
            })
            .count();

        assert_eq!(measured_count, 1, "Exactly one span should be measured");
    }
}
