use std::env::consts::ARCH;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::metrics::ObservableGauge;
use opentelemetry_api::metrics::Unit;
use schemars::JsonSchema;
use serde::Deserialize;
use sysinfo::System;
use tokio::task::JoinHandle;
use tower::BoxError;
use tracing::debug;

use crate::metrics::meter_provider;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {}

#[derive(Debug)]
struct SystemGetter {
    system: System,
    start: Instant,
}

impl SystemGetter {
    fn new() -> Self {
        let mut system = System::new();
        system.refresh_all();
        Self {
            system,
            start: Instant::now(),
        }
    }

    fn get_system(&mut self) -> &System {
        if self.start.elapsed() < REFRESH_INTERVAL {
            &self.system
        } else {
            self.start = Instant::now();
            self.system.refresh_cpu_all();
            self.system.refresh_memory();
            &self.system
        }
    }
}

#[derive(Debug)]
struct FleetDetector {
    handle: JoinHandle<()>,
    #[allow(dead_code)]
    // We have to store a reference to the gauge otherwise it will be dropped once the plugin is
    // initialised, even though it still has data to emit
    freq_gauge: ObservableGauge<u64>,
}

#[async_trait::async_trait]
impl Plugin for FleetDetector {
    type Config = Conf;

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        debug!("beginning environment detection, spawning gauges");
        let system_getter = Arc::new(Mutex::new(SystemGetter::new()));
        let meter = meter_provider().meter("apollo/router");

        let gauge_local_system_getter = system_getter.clone();
        let freq_gauge = meter
            .u64_observable_gauge("apollo.router.instance.cpu_freq")
            .with_description(
                "The CPU frequency of the underlying instance the router is deployed to",
            )
            .with_unit(Unit::new("Mhz"))
            .with_callback(move |i| {
                let mut system_getter = gauge_local_system_getter.lock().unwrap();
                let system = system_getter.get_system();
                let cpus = system.cpus();
                let cpu_freq =
                    cpus.iter().map(|cpu| cpu.frequency()).sum::<u64>() / cpus.len() as u64;
                i.observe(cpu_freq, &[])
            })
            .init();

        debug!("establishing metrics emission task");
        let counter_local_system_getter = system_getter.clone();
        let handle = tokio::task::spawn(async move {
            let mut interval = tokio::time::interval(REFRESH_INTERVAL);
            loop {
                interval.tick().await;
                let mut system_getter = counter_local_system_getter.lock().unwrap();
                let system = system_getter.get_system();
                detect_cpu_values(system);
                detect_memory_values(system);
            }
        });

        Ok(FleetDetector { handle, freq_gauge })
    }
}

impl Drop for FleetDetector {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn detect_cpu_values(system: &System) {
    let cpu_count = detect_cpu_count(system);
    u64_counter!(
        "apollo.router.instance.cpu_count",
        "The number of CPUs reported by the instance the router is running on",
        cpu_count,
        host.arch = get_otel_arch()
    );
}

#[cfg(not(target_os = "linux"))]
fn detect_cpu_count(system: &System) -> u64 {
    system.cpus().len() as u64
}

// Because Linux provides CGroups as a way of controlling the proportion of CPU time each
// process gets we can perform slightly more introspection here than simply appealing to the
// raw number of processors. Hence, the extra logic including below.
#[cfg(target_os = "linux")]
fn detect_cpu_count(system: &System) -> u64 {
    use std::collections::HashSet;
    use std::fs;

    let system_cpus = system.cpus().len() as u64;
    // Grab the contents of /proc/filesystems
    let fses: HashSet<String> = match fs::read_to_string("/proc/filesystems") {
        Ok(content) => content
            .lines()
            .map(|x| x.split_whitespace().next().unwrap_or("").to_string())
            .filter(|x| x.contains("cgroup"))
            .collect(),
        Err(_) => return system_cpus,
    };

    if fses.contains("cgroup2") {
        // If we're looking at cgroup2 then we need to look in `cpu.max`
        match fs::read_to_string("/sys/fs/cgroup/cpu.max") {
            Ok(readings) => {
                // The format of the file lists the quota first, followed by the period,
                // but the quota could also be max which would mean there are no restrictions.
                if readings.starts_with("max") {
                    system_cpus
                } else {
                    // If it's not max then divide the two to get an integer answer
                    let (a, b) = readings.split_once(' ').unwrap();
                    a.parse::<u64>().unwrap() / b.parse::<u64>().unwrap()
                }
            }
            Err(_) => system_cpus,
        }
    } else if fses.contains("cgroup") {
        // If we're in cgroup v1 then we need to read from two separate files
        let quota = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
            .map(|s| String::from(s.trim()))
            .ok();
        let period = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
            .map(|s| String::from(s.trim()))
            .ok();
        match (quota, period) {
            (Some(quota), Some(period)) => {
                // In v1 quota being -1 indicates no restrictions so return the maximum (all
                // system CPUs) otherwise divide the two.
                if quota == "-1" {
                    system_cpus
                } else {
                    quota.parse::<u64>().unwrap() / period.parse::<u64>().unwrap()
                }
            }
            _ => system_cpus,
        }
    } else {
        system_cpus
    }
}

fn detect_memory_values(system: &System) {
    u64_counter!(
        "apollo.router.instance.total_memory",
        "The amount of memory reported by the instance the router is running on",
        system.total_memory()
    );
}

fn get_otel_arch() -> &'static str {
    match ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm32",
        "powerpc" => "ppc32",
        "powerpc64" => "ppc64",
        a => a,
    }
}

register_plugin!("apollo", "fleet_detector", FleetDetector);
