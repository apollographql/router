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

use crate::heap_profiling;
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
    /// Only allocations made after profiling starts are attributed. Linux and macOS only.
    heap: HeapConfig,

    /// How often profiles are collected and exported. Each collection briefly uses extra memory
    /// to dump and symbolize the profile.
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
    /// Whether the last collection failed.
    failing: bool,
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

        if !config.exporters.datadog.enabled {
            return Err("continuous profiling is enabled but no exporter is enabled".into());
        }

        // Don't fail startup where heap profiling isn't supported (e.g. Windows), including on
        // exporter settings that would never be used.
        if let Err(err) = heap_profiling::available() {
            tracing::warn!(error = %err, "heap profiling is enabled but unavailable");
            return Ok(None);
        }

        let exporters = vec![Exporter::new(Box::new(datadog::DatadogExporter::new(
            &config.exporters.datadog,
            env,
        )?))];
        Ok(Some(Self {
            exporters,
            interval: config.interval,
            failing: false,
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

    async fn collect(&mut self, start: SystemTime, end: SystemTime) -> Option<Profile> {
        let result = heap_profiling::collect_pprof().await;
        self.record_collection(result, start, end)
    }

    /// Counts a collection, and logs a failure the way [`export_all`] does, so that collections
    /// that keep failing (e.g. nowhere to write the heap dump) warn once rather than every
    /// interval.
    fn record_collection(
        &mut self,
        result: Result<heap_profiling::HeapProfile, BoxError>,
        start: SystemTime,
        end: SystemTime,
    ) -> Option<Profile> {
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
            Ok(heap) => {
                if self.failing {
                    tracing::info!("collecting heap profiles recovered");
                    self.failing = false;
                }
                Some(Profile {
                    start,
                    end,
                    pprof: heap.pprof,
                })
            }
            Err(err) if self.failing => {
                tracing::debug!(error = %err, "failed to collect heap profile");
                None
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "failed to collect heap profile; repeated failures are logged at debug level until it recovers"
                );
                self.failing = true;
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
    /// The start of the profile being collected. Carried across reloads.
    window_start: Arc<Mutex<SystemTime>>,
    _task: AbortOnDropHandle<()>,
    /// Carried across reloads, so sampling isn't interrupted.
    sampling: heap_profiling::Sampling,
}

static NEXT_PLUGIN_ID: AtomicU64 = AtomicU64::new(0);

/// Start (or with `None`, stop) the collection loop on behalf of the pipeline going live.
fn take_over(owner: u64, collector: Option<Collector>) {
    let mut running = RUNNING.lock();
    let Some(collector) = collector else {
        // Activating the owner again (its collector already taken) leaves its loop running.
        if running.as_ref().is_some_and(|r| r.owner != owner) {
            stop(running.take().expect("checked above"));
        }
        return;
    };
    let previous = running.take();

    let (sampling, window_start) = match previous {
        Some(previous) => (previous.sampling, *previous.window_start.lock()),
        None => match heap_profiling::start_sampling() {
            Ok(sampling) => {
                tracing::info!(interval = ?collector.interval, "continuous profiling started");
                (sampling, SystemTime::now())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to start continuous profiling");
                return;
            }
        },
    };
    let window_start = Arc::new(Mutex::new(window_start));
    let task = tokio::spawn(
        collector
            .run(window_start.clone())
            .with_current_meter_provider(),
    );
    *running = Some(Running {
        owner,
        window_start,
        _task: AbortOnDropHandle::new(task),
        sampling,
    });
}

/// Dropping `running` aborts the loop and releases sampling.
fn stop(running: Running) {
    drop(running);
    tracing::info!("continuous profiling stopped");
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
