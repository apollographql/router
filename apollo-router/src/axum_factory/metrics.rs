#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
pub(crate) mod jemalloc {
    use std::time::Duration;

    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::metrics::ObservableGauge;

    use crate::metrics::meter_provider;

    pub(crate) fn start_epoch_advance_loop() -> tokio::task::JoinHandle<()> {
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

    pub(crate) fn create_active_gauge() -> ObservableGauge<u64> {
        create_jemalloc_gauge!(active, "Total active bytes in jemalloc")
    }

    pub(crate) fn create_allocated_gauge() -> ObservableGauge<u64> {
        create_jemalloc_gauge!(allocated, "Total bytes allocated by jemalloc")
    }

    pub(crate) fn create_metadata_gauge() -> ObservableGauge<u64> {
        create_jemalloc_gauge!(metadata, "Total metadata bytes in jemalloc")
    }

    pub(crate) fn create_mapped_gauge() -> ObservableGauge<u64> {
        create_jemalloc_gauge!(mapped, "Total mapped bytes in jemalloc")
    }

    pub(crate) fn create_resident_gauge() -> ObservableGauge<u64> {
        create_jemalloc_gauge!(resident, "Total resident bytes in jemalloc")
    }

    pub(crate) fn create_retained_gauge() -> ObservableGauge<u64> {
        create_jemalloc_gauge!(retained, "Total retained bytes in jemalloc")
    }
}
