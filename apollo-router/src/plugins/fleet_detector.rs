use schemars::JsonSchema;
use serde::Deserialize;
use sysinfo::System;
use tower::BoxError;
use tracing::debug;
use tracing::info;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;

#[derive(Debug)]
struct FleetDetector {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {}

#[async_trait::async_trait]
impl Plugin for FleetDetector {
    type Config = Conf;

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        debug!("beginning environment detection");
        let sys = &System::new_all();
        let detector = FleetDetector {};
        detector.detect_cpu_values(sys);
        detector.detect_memory_values(sys);
        Ok(detector)
    }
}

impl FleetDetector {
    fn detect_cpu_values(&self, system: &System) {
        let cpus = system.cpus();
        let cpu_freq = cpus.iter().map(|cpu| cpu.frequency()).sum::<u64>() / cpus.len() as u64;
        info!(counter.apollo.router.instance.cpu_freq = cpu_freq);
        let cpu_count = self.detect_cpu_count(system);
        info!(counter.apollo.router.instance.cpu_count = cpu_count);
    }

    #[cfg(not(target_os = "linux"))]
    fn detect_cpu_count(&self, system: &System) -> u64 {
        system.cpus().len() as u64
    }

    #[cfg(target_os = "linux")]
    fn detect_cpu_count(&self, system: &System) -> u64 {
        use std::collections::HashSet;
        use std::fs;

        let system_cpus = system.cpus().len() as u64;
        // Grab the contents of /proc/filesystems
        let filesystems_file = fs::read_to_string("/proc/filesystems").unwrap();
        // Parse the list and only take the first column, filtering to elements that contain
        // 'cgroups'
        let fses: HashSet<&str> = filesystems_file
            .split('\n')
            .map(|x| x.trim().split_whitespace().next().unwrap())
            .filter(|x| x.contains("cgroup"))
            .collect();

        if fses.contains("cgroup2") {
            // If we're looking at cgroup2 then we need to look in `cpu.max`
            match fs::read_to_string("/sys/fs/cgroup/cpu.max") {
                Ok(readings) => {
                    // The format of the file lists the quota first, followed by the period,
                    // but the quote could also be max which would mean there are no restrictions.
                    if readings.starts_with("max") {
                        system_cpus
                    } else {
                        // If it's not max then divide the two to get an integer answer
                        let (a, b) = readings.split_once(" ").unwrap();
                        let result = a.parse::<u64>().unwrap() / b.parse::<u64>().unwrap();
                        result
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
                        let result = quota.parse::<u64>().unwrap() / period.parse::<u64>().unwrap();
                        result
                    }
                }
                _ => system_cpus,
            }
        } else {
            system_cpus
        }
    }

    fn detect_memory_values(&self, system: &System) {
        info!(counter.apollo.router.instance.total_memory = system.total_memory())
    }
}

register_plugin!("apollo", "fleet_detector", FleetDetector);
