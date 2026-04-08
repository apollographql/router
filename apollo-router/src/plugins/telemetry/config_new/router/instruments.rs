use std::fmt::Debug;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Poll;

use bytes::Bytes;
use futures::Stream;
use opentelemetry::metrics::Histogram;
use pin_project_lite::pin_project;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::selectors::RouterSelector;
use super::selectors::RouterValue;
use crate::Context;
use crate::plugins::telemetry::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::ActiveRequestsAttributes;
use crate::plugins::telemetry::config_new::instruments::ActiveRequestsCounter;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::SubscriptionsTerminatedAttributes;
use crate::plugins::telemetry::config_new::instruments::SubscriptionsTerminatedCounter;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::router_overhead::RouterOverheadAttributes;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterInstrumentsConfig {
    /// Histogram of server request duration
    #[serde(rename = "http.server.request.duration")]
    pub(crate) http_server_request_duration:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Counter of active requests
    #[serde(rename = "http.server.active_requests")]
    pub(crate) http_server_active_requests: DefaultedStandardInstrument<ActiveRequestsAttributes>,

    /// Histogram of server request body size
    #[serde(rename = "http.server.request.body.size")]
    pub(crate) http_server_request_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of server response body size
    #[serde(rename = "http.server.response.body.size")]
    pub(crate) http_server_response_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of router overhead (time not spent in subgraph requests). The default unit is
    /// seconds, but this can be changed with selectors
    #[serde(rename = "apollo.router.overhead")]
    pub(crate) router_overhead:
        DefaultedStandardInstrument<Extendable<RouterOverheadAttributes, RouterSelector>>,

    /// Counter of subscriptions terminated
    #[serde(rename = "apollo.router.operations.subscriptions.terminated.client")]
    pub(crate) subscriptions_terminated:
        DefaultedStandardInstrument<Extendable<SubscriptionsTerminatedAttributes, RouterSelector>>,
}

impl DefaultForLevel for RouterInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.http_server_request_duration
            .defaults_for_levels(requirement_level, kind);
        self.http_server_active_requests
            .defaults_for_levels(requirement_level, kind);
        self.http_server_request_body_size
            .defaults_for_levels(requirement_level, kind);
        self.http_server_response_body_size
            .defaults_for_levels(requirement_level, kind);
        self.router_overhead
            .defaults_for_levels(requirement_level, kind);
        self.subscriptions_terminated
            .defaults_for_levels(requirement_level, kind);
    }
}

pub(crate) struct RouterInstruments {
    pub(crate) http_server_request_duration: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) http_server_active_requests: Option<ActiveRequestsCounter>,
    pub(crate) http_server_request_body_size: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) http_server_response_body_size: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) router_overhead: Option<
        CustomHistogram<
            router::Request,
            router::Response,
            (),
            RouterOverheadAttributes,
            RouterSelector,
        >,
    >,
    pub(crate) custom: RouterCustomInstruments,
    pub(crate) subscriptions_terminated: Option<SubscriptionsTerminatedCounter>,
}

impl Instrumented for RouterInstruments {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_request(request);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_request(request);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_request(request);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_request(request);
        }
        if let Some(router_overhead) = &self.router_overhead {
            router_overhead.on_request(request);
        }
        if let Some(subscriptions_terminated_counter) = &self.subscriptions_terminated {
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(subscriptions_terminated_counter.clone()));
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_response(response);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_response(response);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_response(response);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_response(response);
        }
        if let Some(router_overhead) = &self.router_overhead {
            router_overhead.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_error(error, ctx);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_error(error, ctx);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_error(error, ctx);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_error(error, ctx);
        }
        if let Some(router_overhead) = &self.router_overhead {
            router_overhead.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }
}

pub(crate) type RouterCustomInstruments = CustomInstruments<
    router::Request,
    router::Response,
    (),
    RouterAttributes,
    RouterSelector,
    RouterValue,
>;

/// Stashed by `RouterInstruments::on_response` when the `http.server.response.body.size`
/// histogram was not recorded during `on_response` (because compression is pending).
/// Contains the histogram handle and computed attributes so the metric can be recorded
/// later, after the compressed body stream is fully consumed.
pub(crate) struct ResponseBodySizeRecording {
    pub(crate) histogram: Histogram<f64>,
    pub(crate) attributes: Vec<opentelemetry::KeyValue>,
    pub(crate) byte_count: AtomicU64,
}

impl ResponseBodySizeRecording {
    pub(crate) fn new(histogram: Histogram<f64>, attributes: Vec<opentelemetry::KeyValue>) -> Self {
        Self {
            histogram,
            attributes,
            byte_count: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_byte_count(&self, size: u64) {
        self.byte_count.store(size, Ordering::Relaxed);
    }
}

/// Record the `http.server.response.body.size` histogram when dropped,
/// using the final value from `byte_count`. This ensures the metric reflects the
/// actual compressed byte count after the body stream is fully consumed.
impl Drop for ResponseBodySizeRecording {
    fn drop(&mut self) {
        let size = self.byte_count.load(Ordering::Relaxed);
        self.histogram.record(size as f64, &self.attributes);
    }
}

pin_project! {
    /// Stream wrapper that delegates to an inner stream and records the response body
    /// size histogram on drop via the contained `ResponseBodySizeRecording` guard.
    pub(crate) struct ResponseBodySizeRecordingStream<S> {
        #[pin]
        inner: S,
        recording: ResponseBodySizeRecording,
    }
}

impl<S> ResponseBodySizeRecordingStream<S> {
    pub(crate) fn new(inner: S, recording: ResponseBodySizeRecording) -> Self {
        Self { inner, recording }
    }
}

impl<S> Stream for ResponseBodySizeRecordingStream<S>
where
    S: Stream<Item = Result<Bytes, BoxError>>,
{
    type Item = Result<Bytes, BoxError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let next = this.inner.poll_next(cx);
        if let Poll::Ready(Some(Ok(data))) = &next {
            this.recording
                .byte_count
                .fetch_add(data.len() as u64, Ordering::Relaxed);
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use futures::stream;
    use opentelemetry::metrics::MeterProvider;

    use super::*;
    use crate::metrics::FutureMetricsExt;

    fn make_recording(histogram: Histogram<f64>) -> ResponseBodySizeRecording {
        ResponseBodySizeRecording::new(histogram, vec![])
    }

    #[tokio::test]
    async fn recording_stream_accumulates_bytes_across_chunks() {
        async {
            let meter = crate::metrics::meter_provider().meter("test");
            let histogram = meter.f64_histogram("test.body.size").build();

            let chunks: Vec<Result<Bytes, BoxError>> = vec![
                Ok(Bytes::from_static(b"hello")),
                Ok(Bytes::from_static(b" ")),
                Ok(Bytes::from_static(b"world")),
            ];
            let inner = stream::iter(chunks);
            let mut stream = ResponseBodySizeRecordingStream::new(inner, make_recording(histogram));

            let mut collected = Vec::new();
            while let Some(item) = stream.next().await {
                collected.push(item.unwrap());
            }
            assert_eq!(collected.len(), 3);

            assert_eq!(stream.recording.byte_count.load(Ordering::Relaxed), 11);
            drop(stream);

            assert_histogram_sum!("test.body.size", 11);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn recording_stream_records_zero_for_empty_stream() {
        async {
            let meter = crate::metrics::meter_provider().meter("test");
            let histogram = meter.f64_histogram("test.body.size").build();

            let chunks: Vec<Result<Bytes, BoxError>> = vec![];
            let inner = stream::iter(chunks);
            let stream = ResponseBodySizeRecordingStream::new(inner, make_recording(histogram));

            let collected: Vec<_> = stream.collect().await;
            assert!(collected.is_empty());

            assert_histogram_sum!("test.body.size", 0);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn recording_stream_skips_error_chunks_in_byte_count() {
        async {
            let meter = crate::metrics::meter_provider().meter("test");
            let histogram = meter.f64_histogram("test.body.size").build();

            let chunks: Vec<Result<Bytes, BoxError>> = vec![
                Ok(Bytes::from_static(b"abc")),
                Err("simulated error".into()),
                Ok(Bytes::from_static(b"de")),
            ];
            let inner = stream::iter(chunks);
            let mut stream = ResponseBodySizeRecordingStream::new(inner, make_recording(histogram));

            while stream.next().await.is_some() {}
            assert_eq!(stream.recording.byte_count.load(Ordering::Relaxed), 5);
            drop(stream);

            assert_histogram_sum!("test.body.size", 5);
        }
        .with_metrics()
        .await;
    }
}
