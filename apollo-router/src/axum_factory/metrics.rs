#[cfg(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
))]
pub(crate) mod jemalloc {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::metrics::ObservableGauge;

    use crate::metrics::meter_provider;

    pub(crate) fn create_active_gauge() -> ObservableGauge<u64> {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.jemalloc.active")
            .with_description("Total active bytes in jemalloc")
            .with_unit("bytes")
            .with_callback(|gauge| {
                if tikv_jemalloc_ctl::epoch::advance().is_err() {
                    tracing::error!("failed to read jemalloc active stats");
                    return;
                }

                if let Ok(value) = tikv_jemalloc_ctl::stats::active::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::error!("Failed to read jemalloc active stats");
                }
            })
            .init()
    }

    pub(crate) fn create_allocated_gauge() -> ObservableGauge<u64> {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.jemalloc.allocated")
            .with_description("Total bytes allocated by jemalloc")
            .with_unit("bytes")
            .with_callback(|gauge| {
                if tikv_jemalloc_ctl::epoch::advance().is_err() {
                    tracing::error!("failed to read jemalloc allocated stats");
                    return;
                }

                if let Ok(value) = tikv_jemalloc_ctl::stats::allocated::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::error!("Failed to read jemalloc allocated stats");
                }
            })
            .init()
    }

    pub(crate) fn create_metadata_gauge() -> ObservableGauge<u64> {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.jemalloc.metadata")
            .with_description("Total metadata bytes in jemalloc")
            .with_unit("bytes")
            .with_callback(|gauge| {
                if tikv_jemalloc_ctl::epoch::advance().is_err() {
                    tracing::error!("failed to read jemalloc metadata stats");
                    return;
                }

                if let Ok(value) = tikv_jemalloc_ctl::stats::metadata::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::error!("Failed to read jemalloc metadata stats");
                }
            })
            .init()
    }

    pub(crate) fn create_mapped_gauge() -> ObservableGauge<u64> {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.jemalloc.mapped")
            .with_description("Total mapped bytes in jemalloc")
            .with_unit("bytes")
            .with_callback(|gauge| {
                if tikv_jemalloc_ctl::epoch::advance().is_err() {
                    tracing::error!("failed to read jemalloc mapped stats");
                    return;
                }

                if let Ok(value) = tikv_jemalloc_ctl::stats::mapped::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::error!("Failed to read jemalloc mapped stats");
                }
            })
            .init()
    }

    pub(crate) fn create_resident_gauge() -> ObservableGauge<u64> {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.jemalloc.resident")
            .with_description("Total resident bytes in jemalloc")
            .with_unit("bytes")
            .with_callback(|gauge| {
                if tikv_jemalloc_ctl::epoch::advance().is_err() {
                    tracing::error!("failed to read jemalloc resident stats");
                    return;
                }

                if let Ok(value) = tikv_jemalloc_ctl::stats::resident::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::error!("Failed to read jemalloc resident stats");
                }
            })
            .init()
    }

    pub(crate) fn create_retained_gauge() -> ObservableGauge<u64> {
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.jemalloc.retained")
            .with_description("Total retained bytes in jemalloc")
            .with_unit("bytes")
            .with_callback(|gauge| {
                if tikv_jemalloc_ctl::epoch::advance().is_err() {
                    tracing::error!("failed to read jemalloc retained stats");
                    return;
                }

                if let Ok(value) = tikv_jemalloc_ctl::stats::retained::read() {
                    gauge.observe(value as u64, &[]);
                } else {
                    tracing::error!("Failed to read jemalloc retained stats");
                }
            })
            .init()
    }
}
