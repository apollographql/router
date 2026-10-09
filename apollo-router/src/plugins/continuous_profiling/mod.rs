//! Continuous profiling from inside the router binary (experimental).
//!
//! Profiles are collected in-process, encoded as pprof, and handed to exporters on a fixed
//! interval. Because nothing outside the router binary is involved (no external profiler,
//! `LD_PRELOAD`, shared allocator, or `perf_event_open`), this works in any container image,
//! including distroless ones.
//!
//! The shape is intended to grow: profile sources (only the jemalloc heap so far) produce pprof,
//! and exporters (only a Datadog agent so far) ship it. pprof is the common format across
//! profiling backends, so supporting another backend means adding an exporter, not a profiler.

mod datadog;
mod heap;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::task::AbortOnDropHandle;
use tower::BoxError;

use crate::metrics::FutureMetricsExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;

/// Continuous profiling configuration
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "ContinuousProfilingConfig")]
pub(crate) struct Config {
    /// Profile in-use heap memory by allocation call stack.
    ///
    /// Uses the router's jemalloc allocator, which samples allocations, so the overhead is low.
    /// Only allocations made after profiling starts are attributed. Linux only.
    heap: HeapConfig,

    /// How often profiles are collected and exported. Each collection briefly uses extra memory
    /// to symbolize the profile.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    interval: Duration,

    /// Where to export profiles
    exporters: Exporters,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            heap: HeapConfig::default(),
            interval: Duration::from_secs(60),
            exporters: Exporters::default(),
        }
    }
}

/// Heap profiling configuration
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "ContinuousProfilingHeapConfig")]
struct HeapConfig {
    /// Enable heap profiling
    enabled: bool,
}

/// Profile exporters
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "ContinuousProfilingExporters")]
struct Exporters {
    /// Export to a Datadog agent
    datadog: datadog::Config,
}

/// A profile ready to export.
pub(crate) struct Profile {
    pub(crate) start: SystemTime,
    pub(crate) end: SystemTime,
    /// Gzipped pprof
    pub(crate) pprof: Vec<u8>,
}

/// Ships profiles to a backend.
#[async_trait::async_trait]
pub(crate) trait ProfileExporter: Send + Sync {
    fn name(&self) -> &'static str;

    async fn export(&self, profile: &Profile) -> Result<(), BoxError>;
}

/// An exporter, and whether its last export failed.
struct Exporter {
    inner: Box<dyn ProfileExporter>,
    failing: bool,
}

impl Exporter {
    fn new(inner: Box<dyn ProfileExporter>) -> Self {
        Self {
            inner,
            failing: false,
        }
    }
}

/// Collects profiles and exports them on an interval.
struct Collector {
    exporters: Vec<Exporter>,
    interval: Duration,
}

impl Collector {
    /// Returns `None` when no profile source is enabled. `env` looks up an environment variable.
    fn from_config(
        config: &Config,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, BoxError> {
        if !config.heap.enabled {
            return Ok(None);
        }
        if config.interval < Duration::from_secs(1) {
            return Err("continuous profiling interval must be at least 1s".into());
        }

        let mut exporters = Vec::new();
        if config.exporters.datadog.enabled {
            exporters.push(Exporter::new(Box::new(datadog::DatadogExporter::new(
                &config.exporters.datadog,
                env,
            )?)));
        }
        if exporters.is_empty() {
            return Err("continuous profiling is enabled but no exporter is enabled".into());
        }

        // Don't fail startup where heap profiling isn't supported (e.g. a developer's Mac).
        if let Err(err) = heap::available() {
            tracing::warn!(error = %err, "heap profiling is enabled but unavailable");
            return Ok(None);
        }

        Ok(Some(Self {
            exporters,
            interval: config.interval,
        }))
    }

    async fn run(mut self, window_start: Arc<Mutex<SystemTime>>) {
        loop {
            let start = *window_start.lock();
            let due = (start + self.interval)
                .duration_since(SystemTime::now())
                .unwrap_or_default();
            tokio::time::sleep(due).await;

            let end = SystemTime::now();
            if let Some(profile) = self.collect(start, end).await {
                export_all(&mut self.exporters, &profile).await;
            }
            *window_start.lock() = end;
        }
    }

    async fn collect(&self, start: SystemTime, end: SystemTime) -> Option<Profile> {
        // Something else (e.g. the diagnostics plugin) may have switched sampling off.
        if let Ok(false) = heap::is_active() {
            tracing::warn!("jemalloc heap profiling was switched off; switching it back on");
            if let Err(err) = heap::set_active(true) {
                tracing::warn!(error = %err, "failed to switch jemalloc heap profiling on");
            }
        }

        let result = heap::collect_pprof().await;
        let outcome = if result.is_ok() { "success" } else { "failure" };
        u64_counter_with_unit!(
            "apollo.router.profiling.collections",
            "Number of profiles collected by continuous profiling",
            "{profile}",
            1,
            "profiling.type" = "heap",
            "profiling.outcome" = outcome
        );
        match result {
            Ok(pprof) => Some(Profile { start, end, pprof }),
            Err(err) => {
                tracing::warn!(error = %err, "failed to collect heap profile");
                None
            }
        }
    }
}

/// Exports a profile to every exporter. Failures are counted every time, but an exporter that
/// keeps failing (e.g. its agent is down) warns only when it starts failing and logs again when it
/// recovers, rather than warning every interval.
async fn export_all(exporters: &mut [Exporter], profile: &Profile) {
    for exporter in exporters {
        let name = exporter.inner.name();
        let outcome = match exporter.inner.export(profile).await {
            Ok(()) => {
                if exporter.failing {
                    tracing::info!(exporter = name, "exporting heap profiles recovered");
                    exporter.failing = false;
                }
                "success"
            }
            Err(err) if exporter.failing => {
                tracing::debug!(exporter = name, error = %err, "failed to export heap profile");
                "failure"
            }
            Err(err) => {
                tracing::warn!(
                    exporter = name,
                    error = %err,
                    "failed to export heap profile; repeated failures are logged at debug level until it recovers"
                );
                exporter.failing = true;
                "failure"
            }
        };
        u64_counter_with_unit!(
            "apollo.router.profiling.exports",
            "Number of profiles exported by continuous profiling",
            "{profile}",
            1,
            "profiling.type" = "heap",
            "profiling.exporter" = name,
            "profiling.outcome" = outcome
        );
    }
}

/// The process's one collection loop.
///
/// A hot reload builds a new pipeline while the old one may live on for a while (e.g. serving
/// keep-alive connections), so the loop belongs to the process rather than to a plugin instance:
/// the pipeline that goes live takes it over, and only that pipeline's plugin stops it.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    /// The plugin instance that owns the loop.
    owner: u64,
    /// Whether jemalloc heap profiling was already on before we turned it on, in which case we
    /// leave it on when we stop.
    was_active: bool,
    /// The start of the profile being collected. Carried across reloads.
    window_start: Arc<Mutex<SystemTime>>,
    _task: AbortOnDropHandle<()>,
}

static NEXT_PLUGIN_ID: AtomicU64 = AtomicU64::new(0);

/// Start (or with `None`, stop) the collection loop on behalf of the pipeline going live.
fn take_over(owner: u64, collector: Option<Collector>) {
    let mut running = RUNNING.lock();
    let previous = running.take();
    let Some(collector) = collector else {
        if let Some(previous) = previous {
            stop(previous);
        }
        return;
    };

    let (was_active, window_start) = match previous {
        Some(previous) => (previous.was_active, *previous.window_start.lock()),
        None => {
            tracing::info!(interval = ?collector.interval, "continuous profiling started");
            let was_active = heap::is_active().unwrap_or(false);
            if let Err(err) = heap::set_active(true) {
                tracing::warn!(error = %err, "failed to switch jemalloc heap profiling on");
            }
            (was_active, SystemTime::now())
        }
    };
    let window_start = Arc::new(Mutex::new(window_start));
    let task = tokio::spawn(
        collector
            .run(window_start.clone())
            .with_current_meter_provider(),
    );
    *running = Some(Running {
        owner,
        was_active,
        window_start,
        _task: AbortOnDropHandle::new(task),
    });
}

fn stop(running: Running) {
    if !running.was_active
        && let Err(err) = heap::set_active(false)
    {
        tracing::warn!(error = %err, "failed to switch jemalloc heap profiling off");
    }
    tracing::info!("continuous profiling stopped");
    // Dropping `running` aborts the loop.
}

struct ContinuousProfiling {
    id: u64,
    /// Taken when the pipeline goes live, so a pipeline that never activates (e.g. a failed
    /// reload) never touches the running loop.
    collector: Mutex<Option<Collector>>,
}

#[async_trait::async_trait]
impl PluginPrivate for ContinuousProfiling {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            id: NEXT_PLUGIN_ID.fetch_add(1, Ordering::Relaxed),
            collector: Mutex::new(Collector::from_config(&init.config, |key| {
                std::env::var(key).ok()
            })?),
        })
    }

    fn activate(&self) {
        take_over(self.id, self.collector.lock().take());
    }
}

impl Drop for ContinuousProfiling {
    fn drop(&mut self) {
        let mut running = RUNNING.lock();
        if running.as_ref().is_some_and(|r| r.owner == self.id) {
            stop(running.take().expect("checked above"));
        }
    }
}

register_private_plugin!(
    "apollo",
    "experimental_continuous_profiling",
    ContinuousProfiling
);
