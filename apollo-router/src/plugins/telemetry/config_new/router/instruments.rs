use std::fmt::Debug;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Poll;

use bytes::Bytes;
use futures::Stream;
use opentelemetry::metrics::Histogram;
use pin_project_lite::pin_project;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::time::Instant;
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
use crate::plugins::telemetry::config_new::instruments::duration_to_f64;
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

    /// Histogram of the time from request start until the primary response is ready to send
    /// (status, headers, and the first response chunk). Measured inside the router, so this is
    /// not a client-observed time to first byte. Opt-in: disabled by default.
    #[serde(rename = "http.server.request.time_to_first_response")]
    pub(crate) http_server_request_time_to_first_response:
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
        // `http.server.request.time_to_first_response` is opt-in (default off), unlike the
        // other standard router instruments. Only apply attribute defaults once the user has
        // explicitly enabled it, so upgrading does not silently add a new histogram. Leaving
        // it `Unset` keeps `is_enabled()` false and the instrument unbuilt.
        if self.http_server_request_time_to_first_response.is_enabled() {
            self.http_server_request_time_to_first_response
                .defaults_for_levels(requirement_level, kind);
        }
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
    pub(crate) http_server_request_time_to_first_response: Option<
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
        if let Some(http_server_request_time_to_first_response) =
            &self.http_server_request_time_to_first_response
        {
            http_server_request_time_to_first_response.on_request(request);
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
        if let Some(subscriptions_terminated) = &self.subscriptions_terminated {
            subscriptions_terminated.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        // `http.server.request.duration` must reflect the FULL request lifecycle — through
        // the last `@defer` chunk / stream close — not just time-to-first-response. Rather
        // than record here at response-ready, hand the histogram off to a
        // `RequestDurationRecording` stashed in the context; the axum layer records it when
        // the client-facing body stream closes, or on drop if the client hangs mid-stream.
        if let Some(http_server_request_duration) = &self.http_server_request_duration
            && let Some((histogram, attributes, start, unit)) =
                http_server_request_duration.take_duration_recording(response)
        {
            let recording = RequestDurationRecording::new(histogram, attributes, start, unit);
            response
                .context
                .extensions()
                .with_lock(|lock| lock.insert(recording));
        }
        // `http.server.request.time_to_first_response` preserves the old time-to-first-byte
        // signal: same request-start timer, recorded now at response-ready.
        if let Some(http_server_request_time_to_first_response) =
            &self.http_server_request_time_to_first_response
        {
            http_server_request_time_to_first_response.on_response(response);
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
        if let Some(subscriptions_terminated) = &self.subscriptions_terminated {
            subscriptions_terminated.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        // On the error path no response stream is produced, so `take_duration_recording` was
        // never called: the histogram is still live here and records elapsed-at-error.
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_error(error, ctx);
        }
        if let Some(http_server_request_time_to_first_response) =
            &self.http_server_request_time_to_first_response
        {
            http_server_request_time_to_first_response.on_error(error, ctx);
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
        if let Some(subscriptions_terminated) = &self.subscriptions_terminated {
            subscriptions_terminated.on_error(error, ctx);
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

/// Stashed by `RouterInstruments::on_response` so the `http.server.request.duration`
/// histogram can be recorded when the client-facing response stream closes — covering the
/// full request lifecycle including any `@defer` / subscription tail — instead of at
/// response-ready.
///
/// Records the elapsed duration **exactly once**: either when the stream completes normally
/// (via [`Self::record`], driven by [`RequestDurationBody`] on end-of-body) or, failing
/// that, when this guard is dropped — which covers a stream that is cancelled or a client
/// that hangs mid-stream. The [`AtomicBool`] guarantees only the first of the two wins.
pub(crate) struct RequestDurationRecording {
    histogram: Histogram<f64>,
    attributes: Vec<opentelemetry::KeyValue>,
    start: Instant,
    unit: String,
    recorded: AtomicBool,
}

impl RequestDurationRecording {
    pub(crate) fn new(
        histogram: Histogram<f64>,
        attributes: Vec<opentelemetry::KeyValue>,
        start: Instant,
        unit: String,
    ) -> Self {
        Self {
            histogram,
            attributes,
            start,
            unit,
            recorded: AtomicBool::new(false),
        }
    }

    /// Record the elapsed duration since request start, but only on the first call. Safe to
    /// call from both the normal stream-end path and `Drop`; later calls are no-ops.
    pub(crate) fn record(&self) {
        if self
            .recorded
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let value = duration_to_f64(self.start.elapsed(), &self.unit);
            self.histogram.record(value, &self.attributes);
        }
    }
}

/// Record the full-lifecycle request duration if it was not already recorded on normal
/// stream completion — this is the drop-path safety net for cancelled streams and clients
/// that hang mid-stream.
impl Drop for RequestDurationRecording {
    fn drop(&mut self) {
        self.record();
    }
}

pin_project! {
    /// `http_body::Body` wrapper that records `http.server.request.duration` when the
    /// client-facing response body closes. Records on normal end-of-body (`poll_frame`
    /// returns `Ready(None)`); the contained [`RequestDurationRecording`] guard records on
    /// `Drop` if the body is cancelled before completion — together yielding exactly one
    /// sample covering the full request lifecycle.
    ///
    /// Delegates `size_hint`/`is_end_stream` to the inner body so content-length and
    /// streaming semantics are preserved.
    pub(crate) struct RequestDurationBody<B> {
        #[pin]
        inner: B,
        recording: RequestDurationRecording,
    }
}

impl<B> RequestDurationBody<B> {
    pub(crate) fn new(inner: B, recording: RequestDurationRecording) -> Self {
        Self { inner, recording }
    }
}

impl<B> http_body::Body for RequestDurationBody<B>
where
    B: http_body::Body,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let next = this.inner.poll_frame(cx);
        if let Poll::Ready(None) = &next {
            this.recording.record();
        }
        next
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

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

    mod request_duration {
        use http_body::Frame;
        use http_body_util::BodyExt;
        use http_body_util::StreamBody;

        use super::*;

        fn duration_recording(histogram: Histogram<f64>) -> RequestDurationRecording {
            RequestDurationRecording::new(histogram, vec![], Instant::now(), "s".to_string())
        }

        /// A body of `chunk_count` data frames — stands in for a `@defer` / streamed response.
        fn streamed_body(
            chunk_count: usize,
        ) -> StreamBody<impl Stream<Item = Result<Frame<Bytes>, BoxError>>> {
            let frames = (0..chunk_count)
                .map(|_| Ok(Frame::data(Bytes::from_static(b"chunk"))))
                .collect::<Vec<_>>();
            StreamBody::new(stream::iter(frames))
        }

        #[tokio::test]
        async fn records_once_when_body_drains_normally() {
            async {
                let meter = crate::metrics::meter_provider().meter("test");
                let histogram = meter.f64_histogram("test.request.duration").build();

                let body =
                    RequestDurationBody::new(streamed_body(3), duration_recording(histogram));

                // Fully drain the body, like a client reading every `@defer` chunk.
                let collected = body.collect().await.expect("body should drain");
                assert_eq!(collected.to_bytes().len(), 15);

                assert_histogram_count!("test.request.duration", 1);
            }
            .with_metrics()
            .await;
        }

        #[tokio::test]
        async fn records_elapsed_on_drop_when_stream_is_cancelled() {
            async {
                let meter = crate::metrics::meter_provider().meter("test");
                let histogram = meter.f64_histogram("test.request.duration").build();

                let mut body =
                    RequestDurationBody::new(streamed_body(3), duration_recording(histogram));

                // Pull a single frame then drop mid-stream — emulates a client that hangs.
                let first = body.frame().await;
                assert!(first.is_some(), "expected at least one frame");
                drop(body);

                // Still recorded exactly once, via the drop-path safety net.
                assert_histogram_count!("test.request.duration", 1);
            }
            .with_metrics()
            .await;
        }

        #[tokio::test]
        async fn records_exactly_once_across_normal_end_and_drop() {
            async {
                let meter = crate::metrics::meter_provider().meter("test");
                let histogram = meter.f64_histogram("test.request.duration").build();

                let body =
                    RequestDurationBody::new(streamed_body(2), duration_recording(histogram));

                // Drain normally (records via end-of-body) then drop the guard (no-op).
                let _ = body.collect().await.expect("body should drain");

                assert_histogram_count!("test.request.duration", 1);
            }
            .with_metrics()
            .await;
        }

        #[tokio::test]
        async fn does_not_record_until_body_completes() {
            async {
                let meter = crate::metrics::meter_provider().meter("test");
                let histogram = meter.f64_histogram("test.request.duration").build();

                let mut body =
                    RequestDurationBody::new(streamed_body(2), duration_recording(histogram));

                // Read one frame but do not finish the body: nothing recorded yet.
                let _ = body.frame().await;
                // `body` is intentionally kept alive across the assertion below.

                assert_histogram_not_exists!("test.request.duration", f64);
                drop(body);
            }
            .with_metrics()
            .await;
        }
    }
}
