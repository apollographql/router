//! Metered batch span processor that emits metrics when spans are dropped.
//!
//! This is a fork of the OpenTelemetry SDK's `BatchSpanProcessor` that adds:
//! - Named identification of the exporter for better error attribution
//! - Emission of the `apollo.router.telemetry.batch_processor.errors` metric
//!   when spans are dropped due to queue full or channel closed
//!
//! When upstream adds <https://opentelemetry.io/docs/specs/semconv/otel/sdk-metrics/> then this may be removed in the next major router version

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::oneshot;
use futures::executor::block_on;
use futures::future;
use futures::future::BoxFuture;
use futures::future::Either;
use futures::pin_mut;
use futures::select;
use futures::stream;
use futures::stream::FusedStream;
use futures::stream::FuturesUnordered;
use opentelemetry::Context;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::runtime::RuntimeChannel;
use opentelemetry_sdk::runtime::TrySend;
use opentelemetry_sdk::runtime::TrySendError;
use opentelemetry_sdk::trace::Span;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;
use opentelemetry_sdk::trace::SpanProcessor;
use tokio::sync::RwLock;

use crate::plugins::telemetry::reload::tracing::to_interval_stream;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum BatchMessage {
    ExportSpan(SpanData),
    Flush(Option<oneshot::Sender<OTelSdkResult>>),
    Shutdown(oneshot::Sender<OTelSdkResult>),
    SetResource(Arc<Resource>),
}

/// A batch span processor that emits metrics when spans are dropped.
///
/// This is a fork of the OpenTelemetry SDK's `BatchSpanProcessor` with added
/// instrumentation to track dropped spans via the
/// `apollo.router.telemetry.batch_processor.errors` metric.
pub(crate) struct MeteredBatchSpanProcessor<R: RuntimeChannel> {
    message_sender: R::Sender<BatchMessage>,
    dropped_spans_count: AtomicUsize,
    max_queue_size: usize,
    name: &'static str,
}

impl<R: RuntimeChannel> fmt::Debug for MeteredBatchSpanProcessor<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeteredBatchSpanProcessor")
            .field("name", &self.name)
            .field("message_sender", &self.message_sender)
            .finish()
    }
}

impl<R: RuntimeChannel> SpanProcessor for MeteredBatchSpanProcessor<R> {
    fn on_start(&self, _span: &mut Span, _cx: &Context) {
        // Ignored
    }

    fn on_end(&self, span: SpanData) {
        if !span.span_context.is_sampled() {
            return;
        }

        let result = self.message_sender.try_send(BatchMessage::ExportSpan(span));

        if let Err(err) = result {
            let previous_count = self.dropped_spans_count.fetch_add(1, Ordering::Relaxed);

            // Emit metric on every drop, with the appropriate error type
            let error = match err {
                TrySendError::ChannelFull => "channel full",
                TrySendError::ChannelClosed => "channel closed",
                TrySendError::Other(_) => "other",
            };
            emit_batch_processor_error_metric(self.name, error);

            // Log warning on first drop only (matches SDK behavior)
            if previous_count == 0 {
                tracing::warn!(
                    name = self.name,
                    "OpenTelemetry trace warning occurred: Beginning to drop span messages due to full queue. \
                     A metric will be emitted for each dropped span. During shutdown, total dropped count will be logged."
                );
            }
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        let (res_sender, res_receiver) = oneshot::channel();
        self.message_sender
            .try_send(BatchMessage::Flush(Some(res_sender)))
            .map_err(|err| {
                OTelSdkError::InternalFailure(format!(
                    "[{} traces] Failed to send flush message: {err}",
                    self.name
                ))
            })?;

        block_on(res_receiver).map_err(|err| {
            OTelSdkError::InternalFailure(format!(
                "[{} traces] Flush response channel error: {err}",
                self.name
            ))
        })?
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        let dropped_spans = self.dropped_spans_count.load(Ordering::Relaxed);
        let max_queue_size = self.max_queue_size;
        if dropped_spans > 0 {
            tracing::warn!(
                name = self.name,
                dropped_spans = dropped_spans,
                max_queue_size = max_queue_size,
                "OpenTelemetry trace warning occurred: Spans were dropped due to a full queue. \
                 Consider increasing the queue size and/or decreasing delay between intervals."
            );
        }

        let (res_sender, res_receiver) = oneshot::channel();
        self.message_sender
            .try_send(BatchMessage::Shutdown(res_sender))
            .map_err(|err| {
                emit_batch_processor_error_metric(self.name, "channel closed");
                OTelSdkError::InternalFailure(format!(
                    "[{} traces] Failed to send shutdown message: {err}",
                    self.name
                ))
            })?;

        block_on(res_receiver).map_err(|err| {
            OTelSdkError::InternalFailure(format!(
                "[{} traces] Shutdown response channel error: {err}",
                self.name
            ))
        })?
    }

    fn set_resource(&mut self, resource: &Resource) {
        let resource = Arc::new(resource.clone());
        let _ = self
            .message_sender
            .try_send(BatchMessage::SetResource(resource));
    }
}

impl<R: RuntimeChannel> MeteredBatchSpanProcessor<R> {
    pub(crate) fn new<E>(
        exporter: E,
        config: BatchProcessorConfig,
        runtime: R,
        name: &'static str,
    ) -> Self
    where
        E: SpanExporter + Send + Sync + 'static,
    {
        let (message_sender, message_receiver) =
            runtime.batch_message_channel(config.max_queue_size);

        let max_queue_size = config.max_queue_size;

        let inner_runtime = runtime.clone();
        let processor_name = name;
        let config_clone = config.clone();
        runtime.spawn(async move {
            let ticker = to_interval_stream(inner_runtime.clone(), config_clone.scheduled_delay)
                .skip(1)
                .map(|_| BatchMessage::Flush(None));
            let timeout_runtime = inner_runtime.clone();

            let messages = Box::pin(stream::select(message_receiver, ticker));
            let processor = BatchSpanProcessorInternal {
                spans: Vec::new(),
                export_tasks: FuturesUnordered::new(),
                runtime: timeout_runtime,
                config: config_clone,
                exporter: Arc::new(RwLock::new(exporter)),
                name: processor_name,
            };

            processor.run(messages).await
        });

        MeteredBatchSpanProcessor {
            message_sender,
            dropped_spans_count: AtomicUsize::new(0),
            max_queue_size,
            name,
        }
    }

    pub(crate) fn builder<E>(
        exporter: E,
        runtime: R,
        name: &'static str,
    ) -> MeteredBatchSpanProcessorBuilder<E, R>
    where
        E: SpanExporter,
    {
        MeteredBatchSpanProcessorBuilder {
            exporter,
            config: Default::default(),
            runtime,
            name,
        }
    }
}

/// Builder for [`MeteredBatchSpanProcessor`]
#[derive(Debug)]
pub(crate) struct MeteredBatchSpanProcessorBuilder<E, R> {
    exporter: E,
    config: BatchProcessorConfig,
    runtime: R,
    name: &'static str,
}

impl<E, R> MeteredBatchSpanProcessorBuilder<E, R>
where
    E: SpanExporter + 'static,
    R: RuntimeChannel,
{
    pub(crate) fn with_batch_config(self, config: BatchProcessorConfig) -> Self {
        MeteredBatchSpanProcessorBuilder { config, ..self }
    }

    pub(crate) fn build(self) -> MeteredBatchSpanProcessor<R> {
        MeteredBatchSpanProcessor::new(self.exporter, self.config, self.runtime, self.name)
    }
}

struct BatchSpanProcessorInternal<E, R> {
    spans: Vec<SpanData>,
    export_tasks: FuturesUnordered<BoxFuture<'static, OTelSdkResult>>,
    runtime: R,
    config: BatchProcessorConfig,
    exporter: Arc<RwLock<E>>,
    name: &'static str,
}

impl<E: SpanExporter + 'static, R: RuntimeChannel> BatchSpanProcessorInternal<E, R> {
    async fn flush(&mut self, res_channel: Option<oneshot::Sender<OTelSdkResult>>) {
        let name = self.name;
        let export_result = Self::export(
            self.spans.split_off(0),
            self.exporter.clone(),
            self.runtime.clone(),
            self.config.max_export_timeout,
        )
        .await;
        let task = Box::pin(async move {
            if let Some(channel) = res_channel {
                if let Err(result) = channel.send(export_result) {
                    tracing::debug!(
                        name = name,
                        reason = format!("{:?}", result),
                        "Failed to send flush result"
                    );
                }
            } else if let Err(err) = export_result {
                tracing::error!(
                    name = name,
                    reason = format!("{:?}", err),
                    "OpenTelemetry trace error occurred: Failed during the export process"
                );
            }

            Ok(())
        });

        if self.config.max_concurrent_exports == 1 {
            let _ = task.await;
        } else {
            self.export_tasks.push(task);
            while self.export_tasks.next().await.is_some() {}
        }
    }

    async fn process_message(&mut self, message: BatchMessage) -> bool {
        match message {
            BatchMessage::ExportSpan(span) => {
                self.spans.push(span);

                if self.spans.len() == self.config.max_export_batch_size {
                    if !self.export_tasks.is_empty()
                        && self.export_tasks.len() == self.config.max_concurrent_exports
                    {
                        self.export_tasks.next().await;
                    }

                    let batch = self.spans.split_off(0);
                    let exporter = self.exporter.clone();
                    let runtime = self.runtime.clone();
                    let max_export_timeout = self.config.max_export_timeout;
                    let name = self.name;

                    let task = async move {
                        if let Err(err) =
                            Self::export(batch, exporter, runtime, max_export_timeout).await
                        {
                            tracing::error!(
                                name = name,
                                reason = format!("{}", err),
                                "OpenTelemetry trace error occurred: Export failed"
                            );
                        }

                        Ok(())
                    };

                    if self.config.max_concurrent_exports == 1 {
                        let _ = task.await;
                    } else {
                        self.export_tasks.push(Box::pin(task));
                    }
                }
            }
            BatchMessage::Flush(res_channel) => {
                self.flush(res_channel).await;
            }
            BatchMessage::Shutdown(ch) => {
                self.flush(Some(ch)).await;
                let _ = self.exporter.write().await.shutdown();
                return false;
            }
            BatchMessage::SetResource(resource) => {
                self.exporter.write().await.set_resource(&resource);
            }
        }
        true
    }

    async fn export(
        batch: Vec<SpanData>,
        exporter: Arc<RwLock<E>>,
        runtime: R,
        max_export_timeout: Duration,
    ) -> OTelSdkResult {
        if batch.is_empty() {
            return Ok(());
        }

        let exporter_guard = exporter.read().await;
        let export = exporter_guard.export(batch);
        let timeout = runtime.delay(max_export_timeout);

        pin_mut!(export);
        pin_mut!(timeout);

        match future::select(export, timeout).await {
            Either::Left((export_res, _)) => export_res,
            Either::Right((_, _)) => Err(OTelSdkError::Timeout(max_export_timeout)),
        }
    }

    async fn run(mut self, mut messages: impl FusedStream<Item = BatchMessage> + Unpin) {
        loop {
            select! {
                _ = self.export_tasks.next() => {
                    // An export task completed
                },
                message = messages.next() => {
                    match message {
                        Some(message) => {
                            if !self.process_message(message).await {
                                break;
                            }
                        },
                        None => break,
                    }
                },
            }
        }
    }
}

fn emit_batch_processor_error_metric(name: &'static str, error: &'static str) {
    u64_counter!(
        "apollo.router.telemetry.batch_processor.errors",
        "Errors when sending to a batch processor",
        1u64,
        name = name,
        error = error
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::SystemTime;

    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::runtime;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanEvents;
    use opentelemetry_sdk::trace::SpanExporter;
    use opentelemetry_sdk::trace::SpanLinks;
    use opentelemetry_sdk::trace::SpanProcessor;

    use super::*;

    fn create_test_span_data(sampled: bool) -> SpanData {
        let trace_flags = if sampled {
            TraceFlags::SAMPLED
        } else {
            TraceFlags::default()
        };
        let span_context = SpanContext::new(
            TraceId::from(1u128),
            SpanId::from(1u64),
            trace_flags,
            false,
            TraceState::default(),
        );

        SpanData {
            span_context,
            parent_span_id: SpanId::INVALID,
            parent_span_is_remote: false,
            span_kind: SpanKind::Internal,
            name: "test-span".into(),
            start_time: SystemTime::now(),
            end_time: SystemTime::now(),
            attributes: Vec::new(),
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status: opentelemetry::trace::Status::Ok,
            instrumentation_scope: Default::default(),
            dropped_attributes_count: 0,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_sampled_spans_are_exported() {
        let exporter = InMemorySpanExporter::default();

        let config = BatchProcessorConfig {
            max_export_batch_size: 1,
            scheduled_delay: Duration::from_millis(10),
            ..Default::default()
        };

        let processor =
            MeteredBatchSpanProcessor::builder(exporter.clone(), runtime::Tokio, "test")
                .with_batch_config(config)
                .build();

        processor.on_end(create_test_span_data(true));

        // Wait for batch to be processed
        tokio::time::sleep(Duration::from_millis(50)).await;

        let spans = exporter.get_finished_spans().expect("get spans");
        assert!(!spans.is_empty(), "Sampled spans should be exported");

        let _ = processor.shutdown_with_timeout(Duration::from_secs(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_unsampled_spans_are_not_exported() {
        let exporter = InMemorySpanExporter::default();

        let config = BatchProcessorConfig {
            max_export_batch_size: 1,
            scheduled_delay: Duration::from_millis(10),
            ..Default::default()
        };

        let processor =
            MeteredBatchSpanProcessor::builder(exporter.clone(), runtime::Tokio, "test")
                .with_batch_config(config)
                .build();

        processor.on_end(create_test_span_data(false));

        // Wait for batch to be processed
        tokio::time::sleep(Duration::from_millis(50)).await;

        let spans = exporter.get_finished_spans().expect("get spans");
        assert!(spans.is_empty(), "Unsampled spans should not be exported");

        let _ = processor.shutdown_with_timeout(Duration::from_secs(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_queue_full_emits_metric() {
        use crate::metrics::FutureMetricsExt;

        async {
            // Create an exporter that blocks forever (never completes export)
            #[derive(Debug)]
            struct BlockingExporter;

            impl SpanExporter for BlockingExporter {
                fn export(
                    &self,
                    _batch: Vec<SpanData>,
                ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
                    // This future never completes, blocking the export
                    std::future::pending()
                }

                fn shutdown(&mut self) -> OTelSdkResult {
                    Ok(())
                }

                fn force_flush(&mut self) -> OTelSdkResult {
                    Ok(())
                }

                fn set_resource(&mut self, _resource: &opentelemetry_sdk::Resource) {}
            }

            // Queue size of 1, so only 1 span can be queued before drops occur
            let config = BatchProcessorConfig {
                max_queue_size: 1,
                max_export_batch_size: 1,
                scheduled_delay: Duration::from_millis(1),
                ..Default::default()
            };

            let processor = MeteredBatchSpanProcessor::builder(
                BlockingExporter,
                runtime::Tokio,
                "test-exporter",
            )
            .with_batch_config(config)
            .build();

            // Wait for the first span to start being exported (blocking the processor)
            processor.on_end(create_test_span_data(true));
            tokio::time::sleep(Duration::from_millis(20)).await;

            // Now send 5 more spans - these should all be dropped since queue is size 1
            // and export is blocked
            for _ in 0..5 {
                processor.on_end(create_test_span_data(true));
            }

            // Verify the metric was emitted for dropped spans (at least 4 should be dropped)
            assert_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                4,
                "name" = "test-exporter",
                "error" = "channel full"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_successful_export_no_error_metric() {
        use crate::metrics::FutureMetricsExt;

        async {
            let exporter = InMemorySpanExporter::default();

            let config = BatchProcessorConfig {
                max_queue_size: 10,
                max_export_batch_size: 1,
                scheduled_delay: Duration::from_millis(10),
                ..Default::default()
            };

            let processor = MeteredBatchSpanProcessor::builder(
                exporter.clone(),
                runtime::Tokio,
                "success-exporter",
            )
            .with_batch_config(config)
            .build();

            // Send a span that should be successfully exported
            processor.on_end(create_test_span_data(true));

            // Wait for export
            tokio::time::sleep(Duration::from_millis(50)).await;

            let spans = exporter.get_finished_spans().expect("get spans");
            assert!(!spans.is_empty(), "Span should be exported");

            // No error metrics should be emitted for successful exports
            let metrics = crate::metrics::collect_metrics();
            let error_metric = metrics.find("apollo.router.telemetry.batch_processor.errors");
            assert!(
                error_metric.is_none(),
                "No error metrics should be emitted for successful exports"
            );

            let _ = processor.shutdown_with_timeout(Duration::from_secs(1));
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_channel_closed_emits_metric() {
        use crate::metrics::FutureMetricsExt;

        async {
            let exporter = InMemorySpanExporter::default();

            let config = BatchProcessorConfig {
                max_queue_size: 10,
                max_export_batch_size: 1,
                scheduled_delay: Duration::from_millis(10),
                ..Default::default()
            };

            let processor = MeteredBatchSpanProcessor::builder(
                exporter,
                runtime::Tokio,
                "closed-channel-exporter",
            )
            .with_batch_config(config)
            .build();

            // Shutdown the processor - this closes the internal channel
            let _ = processor.shutdown_with_timeout(Duration::from_secs(1));

            // Small delay to ensure shutdown completes
            tokio::time::sleep(Duration::from_millis(10)).await;

            // Now try to send a span after shutdown - this should fail with channel closed
            processor.on_end(create_test_span_data(true));

            // Verify the metric was emitted for the dropped span
            assert_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                1,
                "name" = "closed-channel-exporter",
                "error" = "channel closed"
            );
        }
        .with_metrics()
        .await;
    }
}
