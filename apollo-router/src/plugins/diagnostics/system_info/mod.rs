//! System information collection module
//!
//! This module handles the collection of comprehensive system information
//! for diagnostic purposes, including OS details, CPU info, memory info,
//! build information, and relevant environment variables.

use std::env::consts::ARCH;
use std::env::consts::OS;
use std::time::Duration;

use sysinfo::System;

use crate::plugins::diagnostics::DiagnosticsResult;

/// Collect system information
///
/// SECURITY WARNING: This function collects extensive system information that may be sensitive:
/// - Process IDs, memory layout, CPU details, container environment
/// - Environment variables, filesystem paths, system architecture
/// - Should only be used in development/debugging environments with proper access controls
pub(crate) async fn collect() -> DiagnosticsResult<String> {
    let mut info = String::new();
    info.push_str("SYSTEM INFORMATION\n");
    info.push_str("==================\n\n");

    // Basic system info with better normalization
    info.push_str(&format!(
        "Operating System: {} ({})\n",
        OS,
        get_normalized_os()
    ));
    info.push_str(&format!(
        "Architecture: {} ({})\n",
        ARCH,
        get_normalized_arch()
    ));
    info.push_str(&format!("Target Family: {}\n", std::env::consts::FAMILY));

    // SECURITY NOTE: Process ID exposure can aid in reconnaissance attacks
    // Consider whether this is necessary for diagnostic purposes
    info.push_str(&format!("Process ID: {}\n", std::process::id()));

    // Rust/Cargo info
    info.push_str(&format!("Router Version: {}\n", env!("CARGO_PKG_VERSION")));
    info.push_str(&format!(
        "Rust Version: {}\n",
        env!("CARGO_PKG_RUST_VERSION")
    ));

    // Build and debug information
    collect_build_info(&mut info);

    // SECURITY NOTE: Environment variable exposure
    // Only expose build-related variables, not runtime secrets
    if let Ok(cargo_target_dir) = std::env::var("CARGO_TARGET_DIR") {
        info.push_str(&format!("Cargo Target Dir: {}\n", cargo_target_dir));
    }

    info.push('\n');

    // Create a single System instance for all system info collection
    let mut system = System::new();
    system.refresh_all();

    // Memory information
    info.push_str("MEMORY INFORMATION\n");
    info.push_str("------------------\n");
    collect_memory_info(&mut info, &system);

    info.push('\n');

    // Jemalloc memory statistics (if available) - placed after system memory info
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    {
        info.push_str("JEMALLOC MEMORY STATISTICS\n");
        info.push_str("-------------------------\n");
        collect_jemalloc_stats(&mut info);
        info.push('\n');
    }

    // CPU information
    info.push_str("CPU INFORMATION\n");
    info.push_str("---------------\n");
    collect_cpu_info(&mut info, &system);

    info.push('\n');

    // System uptime and load (cross-platform)
    info.push_str("SYSTEM LOAD\n");
    info.push_str("-----------\n");
    collect_system_load_info(&mut info, &mut system).await;
    info.push('\n');

    // Environment variables
    info.push_str("RELEVANT ENVIRONMENT VARIABLES\n");
    info.push_str("------------------------------\n");
    collect_env_info(&mut info);

    Ok(info)
}

/// Collect memory information using sysinfo for cross-platform support
fn collect_memory_info(info: &mut String, system: &System) {
    let total_memory = system.total_memory();
    let available_memory = system.available_memory();
    let used_memory = system.used_memory();
    let free_memory = system.free_memory();
    let total_swap = system.total_swap();
    let used_swap = system.used_swap();

    info.push_str(&format!(
        "Total Memory: {:.2} GB ({} bytes)\n",
        total_memory as f64 / (1024.0 * 1024.0 * 1024.0),
        total_memory
    ));
    info.push_str(&format!(
        "Available Memory: {:.2} GB ({} bytes)\n",
        available_memory as f64 / (1024.0 * 1024.0 * 1024.0),
        available_memory
    ));
    info.push_str(&format!(
        "Used Memory: {:.2} GB ({} bytes)\n",
        used_memory as f64 / (1024.0 * 1024.0 * 1024.0),
        used_memory
    ));
    info.push_str(&format!(
        "Free Memory: {:.2} GB ({} bytes)\n",
        free_memory as f64 / (1024.0 * 1024.0 * 1024.0),
        free_memory
    ));

    if total_swap > 0 {
        info.push_str(&format!(
            "Total Swap: {:.2} GB ({} bytes)\n",
            total_swap as f64 / (1024.0 * 1024.0 * 1024.0),
            total_swap
        ));
        info.push_str(&format!(
            "Used Swap: {:.2} GB ({} bytes)\n",
            used_swap as f64 / (1024.0 * 1024.0 * 1024.0),
            used_swap
        ));
    } else {
        info.push_str("Swap: Not available or disabled\n");
    }

    // Add Linux-specific detailed info if available
    #[cfg(target_family = "unix")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            info.push_str("\nDetailed Memory Information (Linux /proc/meminfo):\n");
            for line in meminfo.lines().take(15) {
                if line.starts_with("Buffers:")
                    || line.starts_with("Cached:")
                    || line.starts_with("SReclaimable:")
                    || line.starts_with("Shmem:")
                    || line.starts_with("MemAvailable:")
                {
                    info.push_str(&format!("  {}\n", line));
                }
            }
        }
    }
}

/// Collect CPU information using sysinfo and improved cgroup detection
fn collect_cpu_info(info: &mut String, system: &System) {
    let cpus = system.cpus();
    let cpu_count = cpus.len();

    info.push_str(&format!("Physical CPU cores: {}\n", cpu_count));

    if !cpus.is_empty() {
        // Get CPU brand/model from first CPU
        let cpu = &cpus[0];
        info.push_str(&format!("CPU Model: {}\n", cpu.brand()));

        // Calculate average frequency
        let total_freq: u64 = cpus.iter().map(|cpu| cpu.frequency()).sum();
        if total_freq > 0 {
            let avg_freq = total_freq / cpu_count as u64;
            info.push_str(&format!("Average CPU Frequency: {} MHz\n", avg_freq));
        }
    }

    // Thread parallelism info
    match std::thread::available_parallelism() {
        Ok(parallelism) => {
            info.push_str(&format!(
                "Available Parallelism: {} threads\n",
                parallelism.get()
            ));
        }
        Err(_) => {
            info.push_str("Available Parallelism: Unknown\n");
        }
    }

    // Improved container/cgroup CPU information
    collect_enhanced_container_cpu_info(info, system);
}

/// Enhanced container/Kubernetes CPU resource information with improved cgroup detection
fn collect_enhanced_container_cpu_info(info: &mut String, system: &System) {
    info.push_str("\nContainer/Kubernetes CPU Information:\n");

    let system_cpus = system.cpus().len() as u64;
    let (detection_method, effective_cpu_count) = detect_effective_cpu_count(system_cpus);

    info.push_str(&format!(
        "Effective CPU Count: {} (detected via: {})\n",
        effective_cpu_count, detection_method
    ));

    if effective_cpu_count != system_cpus {
        info.push_str(&format!(
            "Note: System reports {} CPUs, but container is limited to {}\n",
            system_cpus, effective_cpu_count
        ));
    }

    // Check CPU shares/weight (relative priority)
    collect_cpu_priority_info(info);

    // Check for Kubernetes downward API environment variables
    collect_k8s_cpu_info(info);

    // Enhanced container detection
    collect_container_environment_info(info);
}

/// Collect system load information (cross-platform)
async fn collect_system_load_info(info: &mut String, system: &mut System) {
    // Use sysinfo's cross-platform load average if available
    let load_avg = System::load_average();
    if load_avg.one.is_finite() && load_avg.one >= 0.0 {
        let cpu_count = system.cpus().len();
        info.push_str(&format!("Load Average (1min): {:.2}\n", load_avg.one));
        info.push_str(&format!("Load Average (5min): {:.2}\n", load_avg.five));
        info.push_str(&format!("Load Average (15min): {:.2}\n", load_avg.fifteen));

        // Add load average interpretation based on CPU count
        if cpu_count > 0 {
            let load_per_cpu_1min = load_avg.one / cpu_count as f64;
            info.push_str(&format!(
                "Load per CPU (1min): {:.2} ({:.1}% utilization)\n",
                load_per_cpu_1min,
                load_per_cpu_1min * 100.0
            ));
        }

        // Also collect individual CPU core usage to identify saturation/hotspots
        // Take initial CPU measurement
        system.refresh_cpu_usage();

        // Wait for CPU usage calculation
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Take second measurement
        system.refresh_cpu_usage();

        info.push_str("Individual CPU Core Usage:\n");
        let mut total_usage = 0.0f32;
        let cpus = system.cpus();

        for (i, cpu) in cpus.iter().enumerate() {
            let usage = cpu.cpu_usage();
            total_usage += usage;
            info.push_str(&format!("  CPU {}: {:.1}%\n", i, usage));
        }

        if !cpus.is_empty() {
            let avg_usage = total_usage / cpus.len() as f32;
            info.push_str(&format!("Average CPU Usage: {:.1}%\n", avg_usage));
        }
    } else {
        // Fallback: Calculate CPU usage over time for cross-platform load estimation
        // Take initial CPU measurement
        system.refresh_cpu_usage();

        // Wait for CPU usage calculation
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Take second measurement
        system.refresh_cpu_usage();

        info.push_str("Load Average: Not available on this platform\n");
        info.push_str("CPU Usage (per core):\n");

        let mut total_usage = 0.0f32;
        let cpus = system.cpus();

        for (i, cpu) in cpus.iter().enumerate() {
            let usage = cpu.cpu_usage();
            total_usage += usage;
            info.push_str(&format!("  CPU {}: {:.1}%\n", i, usage));
        }

        if !cpus.is_empty() {
            let avg_usage = total_usage / cpus.len() as f32;
            info.push_str(&format!("Total CPU Usage (average): {:.1}%\n", avg_usage));
        }
    }
}

/// Collect build and debug symbol information
fn collect_build_info(info: &mut String) {
    info.push_str("\nBUILD INFORMATION\n");
    info.push_str("-----------------\n");

    // Check if running in debug vs release mode
    if cfg!(debug_assertions) {
        info.push_str("Build Type: Debug (with debug assertions)\n");
    } else {
        info.push_str("Build Type: Release (optimized)\n");
    }

    // Build profile information
    if let Ok(profile) = std::env::var("CARGO_BUILD_PROFILE") {
        info.push_str(&format!("Build Profile: {}\n", profile));
    }

    // Target information
    if let Ok(target_triple) = std::env::var("CARGO_CFG_TARGET_TRIPLE") {
        info.push_str(&format!("Target Triple: {}\n", target_triple));
    }
    if let Ok(opt_level) = std::env::var("CARGO_CFG_OPT_LEVEL") {
        info.push_str(&format!("Optimization Level: {}\n", opt_level));
    }
}

/// Collect jemalloc memory statistics if available
#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
fn collect_jemalloc_stats(info: &mut String) {
    // Advance epoch to get fresh statistics
    if let Err(e) = tikv_jemalloc_ctl::epoch::advance() {
        info.push_str(&format!("Error advancing jemalloc epoch: {}\n", e));
        return;
    }

    // Helper macro to read and format jemalloc stats
    macro_rules! add_stat {
        ($stat:ident, $description:expr) => {
            match tikv_jemalloc_ctl::stats::$stat::read() {
                Ok(value) => {
                    info.push_str(&format!(
                        "{}: {:.2} MB ({} bytes)\n",
                        $description,
                        value as f64 / (1024.0 * 1024.0),
                        value
                    ));
                }
                Err(e) => {
                    info.push_str(&format!("{}: Error reading - {}\n", $description, e));
                }
            }
        };
    }

    // Core jemalloc memory statistics (these match the metrics already collected)
    add_stat!(allocated, "Allocated Memory");
    add_stat!(active, "Active Memory");
    add_stat!(mapped, "Mapped Memory");
    add_stat!(retained, "Retained Memory");
    add_stat!(resident, "Resident Memory");
    add_stat!(metadata, "Metadata Memory");

    // Additional detailed statistics if available
    if let Ok(arenas) = tikv_jemalloc_ctl::arenas::narenas::read() {
        info.push_str(&format!("Number of Arenas: {}\n", arenas));
    }

    // Memory usage efficiency indicators
    if let (Ok(allocated), Ok(resident)) = (
        tikv_jemalloc_ctl::stats::allocated::read(),
        tikv_jemalloc_ctl::stats::resident::read(),
    ) {
        if resident > 0 {
            let efficiency = (allocated as f64 / resident as f64) * 100.0;
            info.push_str(&format!(
                "Memory Efficiency: {:.1}% (allocated/resident)\n",
                efficiency
            ));
        }
    }

    if let (Ok(active), Ok(mapped)) = (
        tikv_jemalloc_ctl::stats::active::read(),
        tikv_jemalloc_ctl::stats::mapped::read(),
    ) {
        if mapped > 0 {
            let utilization = (active as f64 / mapped as f64) * 100.0;
            info.push_str(&format!(
                "Memory Utilization: {:.1}% (active/mapped)\n",
                utilization
            ));
        }
    }
}

/// Collect relevant environment variables
///
/// SECURITY: Limited environment variable exposure
/// Only expose specifically allowlisted environment variables that are safe for diagnostics.
/// Never expose variables that might contain secrets, tokens, passwords, or API keys.
fn collect_env_info(info: &mut String) {
    // SECURITY: Explicit allowlist of safe environment variables
    // Only Apollo-specific configuration variables are included
    // DO NOT add variables that might contain secrets (API keys, passwords, etc.)
    let relevant_vars = ["APOLLO_GRAPH_REF"];

    for var in &relevant_vars {
        if let Ok(value) = std::env::var(var) {
            // SECURITY: Truncate long values to prevent log injection or excessive output
            let display_value = if value.len() > 200 {
                format!(
                    "{}... (truncated, {} chars total)",
                    &value[..200],
                    value.len()
                )
            } else {
                value
            };
            info.push_str(&format!("{}: {}\n", var, display_value));
        }
    }

    // If no relevant environment variables are set, indicate that
    if !relevant_vars.iter().any(|var| std::env::var(var).is_ok()) {
        info.push_str("No relevant Apollo environment variables set\n");
    }
}

/// Get normalized OS name for better cross-platform reporting
pub(crate) fn get_normalized_os() -> &'static str {
    match OS {
        "apple" => "darwin",
        "dragonfly" => "dragonflybsd",
        "macos" => "darwin",
        "ios" => "darwin",
        os => os,
    }
}

/// Get normalized architecture name for better cross-platform reporting
pub(crate) fn get_normalized_arch() -> &'static str {
    match ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm32",
        "powerpc" => "ppc32",
        "powerpc64" => "ppc64",
        arch => arch,
    }
}

/// Detect effective CPU count considering cgroup limits (improved from fleet detector)
#[cfg(not(target_os = "linux"))]
fn detect_effective_cpu_count(system_cpus: u64) -> (&'static str, u64) {
    ("system", system_cpus)
}

#[cfg(target_os = "linux")]
fn detect_effective_cpu_count(system_cpus: u64) -> (&'static str, u64) {
    // Determine cgroup version first
    match std::fs::read_to_string("/proc/filesystems").map(|fs| detect_cgroup_version(&fs)) {
        Ok(CGroupVersion::CGroup2) => {
            // cgroup v2: read from cpu.max
            match std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
                Ok(contents) => {
                    if contents.starts_with("max") {
                        ("system", system_cpus)
                    } else {
                        match contents.split_once(' ') {
                            Some((quota, period)) => (
                                "cgroup2",
                                calculate_cpu_count_with_default(system_cpus, quota, period),
                            ),
                            None => ("system", system_cpus),
                        }
                    }
                }
                Err(_) => ("system", system_cpus),
            }
        }
        Ok(CGroupVersion::CGroup) => {
            // cgroup v1: read from separate quota and period files
            let quota = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
                .map(|s| s.trim().to_string())
                .ok();
            let period = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
                .map(|s| s.trim().to_string())
                .ok();

            match (quota, period) {
                (Some(quota), Some(period)) => {
                    if quota == "-1" {
                        ("system", system_cpus)
                    } else {
                        (
                            "cgroup",
                            calculate_cpu_count_with_default(system_cpus, &quota, &period),
                        )
                    }
                }
                _ => ("system", system_cpus),
            }
        }
        _ => ("system", system_cpus),
    }
}

/// Detect cgroup version from /proc/filesystems content
#[cfg(target_os = "linux")]
fn detect_cgroup_version(filesystems: &str) -> CGroupVersion {
    use std::collections::HashSet;

    let versions: HashSet<_> = filesystems
        .lines()
        .flat_map(|line| line.split_whitespace())
        .filter(|word| word.contains("cgroup"))
        .collect();

    if versions.contains("cgroup2") {
        CGroupVersion::CGroup2
    } else if versions.contains("cgroup") {
        CGroupVersion::CGroup
    } else {
        CGroupVersion::None
    }
}

/// CGroup version enumeration
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CGroupVersion {
    CGroup2,
    CGroup,
    None,
}

/// Calculate CPU count from quota and period with fallback
#[cfg(target_os = "linux")]
fn calculate_cpu_count_with_default(default: u64, quota: &str, period: &str) -> u64 {
    match (quota.parse::<u64>(), period.parse::<u64>()) {
        (Ok(q), Ok(p)) if p > 0 => q / p,
        _ => default,
    }
}

/// Collect CPU priority information (shares/weight)
fn collect_cpu_priority_info(info: &mut String) {
    #[cfg(target_os = "linux")]
    {
        // cgroup v1 shares
        if let Ok(cpu_shares) = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.shares") {
            let shares = cpu_shares.trim();
            info.push_str(&format!("CPU Shares (cgroup v1): {}\n", shares));
        }
        // cgroup v2 weight
        else if let Ok(cpu_weight) = std::fs::read_to_string("/sys/fs/cgroup/cpu.weight") {
            let weight = cpu_weight.trim();
            info.push_str(&format!("CPU Weight (cgroup v2): {}\n", weight));
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = info; // Suppress unused variable warning
    }
}

/// Collect Kubernetes CPU resource information
fn collect_k8s_cpu_info(info: &mut String) {
    if let Ok(cpu_request) = std::env::var("CPU_REQUEST") {
        info.push_str(&format!("CPU Request (K8s): {}\n", cpu_request));
    }
    if let Ok(cpu_limit) = std::env::var("CPU_LIMIT") {
        info.push_str(&format!("CPU Limit (K8s): {}\n", cpu_limit));
    }
}

/// Enhanced container environment detection
fn collect_container_environment_info(info: &mut String) {
    let mut container_indicators = Vec::new();

    // Docker
    if std::path::Path::new("/.dockerenv").exists() {
        container_indicators.push("Docker");
    }

    // Podman
    if std::path::Path::new("/run/.containerenv").exists() {
        container_indicators.push("Podman");
    }

    // Generic container indicators
    if std::env::var("container").is_ok() {
        container_indicators.push("Generic Container");
    }

    // Kubernetes
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        container_indicators.push("Kubernetes");
    }

    if container_indicators.is_empty() {
        info.push_str("Container Environment: Not detected (likely bare metal/VM)\n");
    } else {
        info.push_str(&format!(
            "Container Environment: {} detected\n",
            container_indicators.join(", ")
        ));
    }
}

#[cfg(test)]
mod tests;
