//! Tower layer that publishes jemalloc allocator metrics for as long as the
//! wrapped service is alive.
//!
//! This isn't a "real" layer in the sense of inspecting or transforming
//! requests/responses — [`JemallocMetricsService`] just forwards every call
//! straight to the inner service. The jemalloc gauges self-report via
//! OpenTelemetry callbacks and the epoch-advance task runs on its own timer,
//! so neither one needs to sit in the request path.
//!
//! It's structured as a layer purely to piggyback on the router's lifetime:
//! `main_router` has no other natural "this router is gone" hook to attach
//! cleanup to. Wrapping the service stack with this layer means the
//! instruments (gauges + epoch-advance task) are kept alive by an `Arc`
//! shared with every clone of the service, and get torn down automatically
//! once the last clone is dropped (e.g. on config reload), instead of
//! leaking for the lifetime of the process.
//!
//! ## Ordering with respect to the telemetry plugin
//!
//! [`create_gauges`] calls [`meter_provider`], which is always safe to call
//! (it lazily falls back to a default provider), but the gauges' callbacks
//! are only useful if they end up registered against the *real* provider the
//! telemetry plugin installs. That registration is **not** self-healing per
//! instrument: when the telemetry plugin swaps in a new provider it calls
//! `AggregateMeterProvider::set`, which wipes every previously-registered
//! observable callback outright (see `clear_provider` in
//! `crate::metrics::aggregation`) rather than migrating them to the new
//! provider. Metrics only keep flowing because the whole plugin/service
//! graph — this layer included — is rebuilt from scratch immediately after,
//! which re-runs [`create_gauges`] and re-registers fresh callbacks.
//!
//! So this layer relies on an invariant enforced elsewhere, not here: this
//! module must not be the *last* thing built before a provider swap without
//! also being rebuilt afterwards. In practice that holds because:
//! - on cold start, `telemetry.activate()` runs (and calls
//!   `AggregateMeterProvider::set`) before the rest of the plugins and the
//!   router are built, so the provider is already final by the time this
//!   layer's gauges are created; and
//! - on every hot reload, a brand-new `RouterFactory` — and therefore a
//!   brand-new `main_router` and a brand-new instance of this layer — is
//!   built after any provider swap, satisfying the "whole graph gets
//!   rebuilt" assumption `clear_provider` depends on.
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use tower::Layer;
use tower::Service;

use crate::metrics::meter_provider;

fn start_epoch_advance_loop() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tikv_jemalloc_ctl::epoch::advance() {
                tracing::warn!("Failed to advance jemalloc epoch: {}", e);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}

macro_rules! create_jemalloc_gauge {
    ($name:ident, $description:expr) => {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge(concat!("apollo.router.jemalloc.", stringify!($name)))
            .with_description($description)
            .with_unit("bytes")
            .with_callback(|gauge| {
                if let Ok(value) = tikv_jemalloc_ctl::stats::$name::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::warn!("Failed to read jemalloc {} stats", stringify!($name));
                }
            })
            .build()
    };
}

fn create_gauges() -> Vec<ObservableGauge<u64>> {
    vec![
        create_jemalloc_gauge!(active, "Total active bytes in jemalloc"),
        create_jemalloc_gauge!(allocated, "Total bytes allocated by jemalloc"),
        create_jemalloc_gauge!(metadata, "Total metadata bytes in jemalloc"),
        create_jemalloc_gauge!(mapped, "Total mapped bytes in jemalloc"),
        create_jemalloc_gauge!(resident, "Total resident bytes in jemalloc"),
        create_jemalloc_gauge!(retained, "Total retained bytes in jemalloc"),
    ]
}

/// Owns the epoch-advance task and the observable gauges. Both are
/// self-driving (the task runs on its own timer, the gauges report via
/// OpenTelemetry callbacks) — this struct exists only to keep them alive and
/// to stop them together, via `Drop`, once nothing references it any more.
struct JemallocInstruments {
    _epoch_advance_loop: tokio::task::JoinHandle<()>,
    _gauges: Vec<ObservableGauge<u64>>,
}

impl JemallocInstruments {
    fn new() -> Self {
        Self {
            _epoch_advance_loop: start_epoch_advance_loop(),
            _gauges: create_gauges(),
        }
    }
}

impl Drop for JemallocInstruments {
    fn drop(&mut self) {
        // Dropping a `JoinHandle` alone does not stop the task, it only
        // detaches it, so the epoch-advance loop must be aborted explicitly.
        self._epoch_advance_loop.abort();
    }
}

/// A pass-through layer that ties the lifetime of the jemalloc metrics
/// instruments to the lifetime of the service stack it's applied to. See the
/// module docs for why a `Layer` is used for this at all.
#[derive(Clone)]
pub(crate) struct JemallocMetricsLayer {
    instruments: Arc<JemallocInstruments>,
}

impl JemallocMetricsLayer {
    pub(crate) fn new() -> Self {
        Self {
            instruments: Arc::new(JemallocInstruments::new()),
        }
    }
}

impl<S> Layer<S> for JemallocMetricsLayer {
    type Service = JemallocMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        JemallocMetricsService {
            inner,
            _instruments: self.instruments.clone(),
        }
    }
}

/// Forwards every call to `inner` unchanged; the `_instruments` field's only
/// job is to keep the jemalloc gauges/task alive for as long as this service
/// (or a clone of it) exists.
#[derive(Clone)]
pub(crate) struct JemallocMetricsService<S> {
    inner: S,
    _instruments: Arc<JemallocInstruments>,
}

impl<S, Request> Service<Request> for JemallocMetricsService<S>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn it_passes_calls_through_to_the_inner_service() {
        let layer = JemallocMetricsLayer::new();
        let (inner, mut handle) = tower_test::mock::pair::<u32, u32>();
        handle.allow(1);
        let driver = tokio::spawn(async move {
            let (request, respond) = handle
                .next_request()
                .await
                .expect("service should be called");
            respond.send_response(request * 2);
        });
        let mut service = layer.layer(inner);

        let response = service.ready().await.unwrap().call(21).await.unwrap();

        assert_eq!(response, 42);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn it_stops_the_epoch_advance_loop_once_every_clone_is_dropped() {
        let layer = JemallocMetricsLayer::new();
        let abort_handle = layer.instruments._epoch_advance_loop.abort_handle();
        let (inner, handle) = tower_test::mock::pair::<u32, u32>();
        let service = layer.layer(inner);
        let cloned_service = service.clone();

        drop(layer);
        drop(service);
        assert!(
            !abort_handle.is_finished(),
            "the loop should still be running while a clone of the service is alive"
        );

        drop(cloned_service);
        // Give the runtime a chance to actually cancel the aborted task.
        tokio::task::yield_now().await;

        assert!(
            abort_handle.is_finished(),
            "the loop should stop once every instance sharing the instruments is dropped"
        );
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }
}
