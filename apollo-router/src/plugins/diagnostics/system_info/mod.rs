//! System information collection module
//!
//! This module handles the collection of comprehensive system information
//! for diagnostic purposes, including OS details, CPU info, memory info,
//! build information, and relevant environment variables.

use std::env::consts::ARCH;
use std::env::consts::OS;
use std::fmt;
use std::time::Duration;

use sysinfo::System;

use crate::plugins::diagnostics::DiagnosticsResult;

/// Complete system diagnostic information
struct SystemDiagnostics {
    basic_system: BasicSystemInfo,
    rust: RustInfo,
    memory: MemoryInfo,
    jemalloc: JemallocInfo,
    cpu: CpuInfo,
    system_load: SystemLoadInfo,
    environment: EnvironmentInfo,
}

impl SystemDiagnostics {
    /// Collect all system diagnostic information
    async fn new() -> Self {
        // Create a single System instance for all system info collection
        let mut system = System::new_all();

        Self {
            basic_system: BasicSystemInfo::new().await,
            rust: RustInfo::new(),
            memory: MemoryInfo::new(&system).await,
            jemalloc: JemallocInfo::new(),
            cpu: CpuInfo::new(&system).await,
            system_load: SystemLoadInfo::new(&mut system).await,
            environment: EnvironmentInfo::new(),
        }
    }
}

impl fmt::Display for SystemDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}{}{}{}{}",
            self.basic_system,
            self.rust,
            self.memory,
            self.jemalloc,
            self.cpu,
            self.system_load,
            self.environment
        )
    }
}

/// Basic system information
struct BasicSystemInfo {
    operating_system: &'static str,
    architecture: &'static str,
    target_family: &'static str,
    container_environment: Vec<String>,
}

impl BasicSystemInfo {
    /// Collect basic system information
    async fn new() -> Self {
        let container_environment = Self::collect_container_environment_info().await;
        Self {
            operating_system: OS,
            architecture: ARCH,
            target_family: std::env::consts::FAMILY,
            container_environment,
        }
    }

    /// Enhanced container environment detection
    async fn collect_container_environment_info() -> Vec<String> {
        let mut container_indicators = Vec::new();

        // Docker
        if std::path::Path::new("/.dockerenv").exists() {
            container_indicators.push("Docker".to_string());
        }

        // Podman
        if std::path::Path::new("/run/.containerenv").exists() {
            container_indicators.push("Podman".to_string());
        }

        // Generic container indicators
        if std::env::var("container").is_ok() {
            container_indicators.push("Generic Container".to_string());
        }

        // Kubernetes
        if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
            container_indicators.push("Kubernetes".to_string());
        }

        container_indicators
    }
}

impl fmt::Display for BasicSystemInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "SYSTEM INFORMATION")?;
        writeln!(f, "==================")?;
        writeln!(f)?;
        writeln!(f, "Operating System: {}", self.operating_system)?;
        writeln!(f, "Architecture: {}", self.architecture)?;
        writeln!(f, "Target Family: {}", self.target_family)?;

        if self.container_environment.is_empty() {
            writeln!(
                f,
                "Container Environment: Not detected (likely bare metal/VM)"
            )?;
        } else {
            writeln!(
                f,
                "Container Environment: {} detected",
                self.container_environment.join(", ")
            )?;
        }

        writeln!(f)
    }
}

/// Rust and build information
struct RustInfo {
    router_version: &'static str,
    rust_version: &'static str,
    build: BuildInfo,
}

impl RustInfo {
    /// Collect Rust and Cargo information
    fn new() -> Self {
        Self {
            router_version: env!("CARGO_PKG_VERSION"),
            rust_version: env!("CARGO_PKG_RUST_VERSION"),
            build: BuildInfo::new(),
        }
    }
}

impl fmt::Display for RustInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Router Version: {}", self.router_version)?;
        writeln!(f, "Rust Version: {}", self.rust_version)?;
        writeln!(f, "{}", self.build)?;
        writeln!(f)
    }
}

/// Build and debug information
struct BuildInfo {
    build_type: &'static str,
    build_profile: Option<String>,
    target_triple: Option<String>,
    optimization_level: Option<String>,
}

impl BuildInfo {
    /// Collect build and debug symbol information
    fn new() -> Self {
        // Check if running in debug vs release mode
        let build_type = if cfg!(debug_assertions) {
            "Debug (with debug assertions)"
        } else {
            "Release (optimized)"
        };

        // Build profile information
        let build_profile = std::env::var("CARGO_BUILD_PROFILE").ok();

        // Target information
        let target_triple = std::env::var("CARGO_CFG_TARGET_TRIPLE").ok();
        let optimization_level = std::env::var("CARGO_CFG_OPT_LEVEL").ok();

        Self {
            build_type,
            build_profile,
            target_triple,
            optimization_level,
        }
    }
}

impl fmt::Display for BuildInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f)?;
        writeln!(f, "BUILD INFORMATION")?;
        writeln!(f, "-----------------")?;
        writeln!(f, "Build Type: {}", self.build_type)?;

        if let Some(ref profile) = self.build_profile {
            writeln!(f, "Build Profile: {}", profile)?;
        }
        if let Some(ref target) = self.target_triple {
            writeln!(f, "Target Triple: {}", target)?;
        }
        if let Some(ref opt_level) = self.optimization_level {
            writeln!(f, "Optimization Level: {}", opt_level)?;
        }
        Ok(())
    }
}

/// System memory information
struct MemoryInfo {
    total_memory: u64,
    available_memory: u64,
    used_memory: u64,
    free_memory: u64,
    total_swap: u64,
    used_swap: u64,
    detailed_linux_info: Option<String>,
}

impl MemoryInfo {
    /// Collect memory information using sysinfo for cross-platform support
    async fn new(system: &System) -> Self {
        let total_memory = system.total_memory();
        let available_memory = system.available_memory();
        let used_memory = system.used_memory();
        let free_memory = system.free_memory();
        let total_swap = system.total_swap();
        let used_swap = system.used_swap();

        // Add Linux-specific detailed info if available
        #[cfg(target_family = "unix")]
        let detailed_linux_info = {
            if let Ok(meminfo) = tokio::fs::read_to_string("/proc/meminfo").await {
                let mut detailed = String::new();
                for line in meminfo.lines().take(15) {
                    if line.starts_with("Buffers:")
                        || line.starts_with("Cached:")
                        || line.starts_with("SReclaimable:")
                        || line.starts_with("Shmem:")
                        || line.starts_with("MemAvailable:")
                    {
                        detailed.push_str(&format!("  {}\n", line));
                    }
                }
                Some(detailed)
            } else {
                None
            }
        };

        #[cfg(not(target_family = "unix"))]
        let detailed_linux_info = None;

        Self {
            total_memory,
            available_memory,
            used_memory,
            free_memory,
            total_swap,
            used_swap,
            detailed_linux_info,
        }
    }
}

impl fmt::Display for MemoryInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "MEMORY INFORMATION")?;
        writeln!(f, "------------------")?;

        writeln!(
            f,
            "Total Memory: {:.2} GB ({} bytes)",
            self.total_memory as f64 / (1024.0 * 1024.0 * 1024.0),
            self.total_memory
        )?;
        writeln!(
            f,
            "Available Memory: {:.2} GB ({} bytes)",
            self.available_memory as f64 / (1024.0 * 1024.0 * 1024.0),
            self.available_memory
        )?;
        writeln!(
            f,
            "Used Memory: {:.2} GB ({} bytes)",
            self.used_memory as f64 / (1024.0 * 1024.0 * 1024.0),
            self.used_memory
        )?;
        writeln!(
            f,
            "Free Memory: {:.2} GB ({} bytes)",
            self.free_memory as f64 / (1024.0 * 1024.0 * 1024.0),
            self.free_memory
        )?;

        if self.total_swap > 0 {
            writeln!(
                f,
                "Total Swap: {:.2} GB ({} bytes)",
                self.total_swap as f64 / (1024.0 * 1024.0 * 1024.0),
                self.total_swap
            )?;
            writeln!(
                f,
                "Used Swap: {:.2} GB ({} bytes)",
                self.used_swap as f64 / (1024.0 * 1024.0 * 1024.0),
                self.used_swap
            )?;
        } else {
            writeln!(f, "Swap: Not available or disabled")?;
        }

        if let Some(ref detailed_info) = self.detailed_linux_info {
            writeln!(f, "\nDetailed Memory Information (Linux /proc/meminfo):")?;
            write!(f, "{}", detailed_info)?;
        } else {
            writeln!(
                f,
                "\nDetailed Memory Information: not available on this platform"
            )?;
        }

        writeln!(f)
    }
}

/// Jemalloc memory statistics
struct JemallocInfo {
    available: bool,
    allocated: Option<u64>,
    active: Option<u64>,
    mapped: Option<u64>,
    retained: Option<u64>,
    resident: Option<u64>,
    metadata: Option<u64>,
    arenas: Option<u64>,
    memory_efficiency: Option<f64>,
    memory_utilization: Option<f64>,
}

impl JemallocInfo {
    /// Collect jemalloc memory statistics
    fn new() -> Self {
        #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
        {
            Self::collect_jemalloc_stats()
        }

        #[cfg(not(all(feature = "global-allocator", not(feature = "dhat-heap"), unix)))]
        {
            Self {
                available: false,
                allocated: None,
                active: None,
                mapped: None,
                retained: None,
                resident: None,
                metadata: None,
                arenas: None,
                memory_efficiency: None,
                memory_utilization: None,
            }
        }
    }

    /// Collect jemalloc memory statistics if available
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    fn collect_jemalloc_stats() -> Self {
        // Advance epoch to get fresh statistics
        if let Err(_e) = tikv_jemalloc_ctl::epoch::advance() {
            return Self {
                available: true,
                allocated: None,
                active: None,
                mapped: None,
                retained: None,
                resident: None,
                metadata: None,
                arenas: None,
                memory_efficiency: None,
                memory_utilization: None,
            };
        }

        // Helper macro to read jemalloc stats
        macro_rules! read_stat {
            ($stat:ident) => {
                tikv_jemalloc_ctl::stats::$stat::read().ok()
            };
        }

        let allocated = read_stat!(allocated).map(|v| v as u64);
        let active = read_stat!(active).map(|v| v as u64);
        let mapped = read_stat!(mapped).map(|v| v as u64);
        let retained = read_stat!(retained).map(|v| v as u64);
        let resident = read_stat!(resident).map(|v| v as u64);
        let metadata = read_stat!(metadata).map(|v| v as u64);

        // Additional detailed statistics if available
        let arenas = tikv_jemalloc_ctl::arenas::narenas::read()
            .ok()
            .map(|v| v as u64);

        // Memory usage efficiency indicators
        let memory_efficiency = match (allocated, resident) {
            (Some(alloc), Some(res)) if res > 0 => Some((alloc as f64 / res as f64) * 100.0),
            _ => None,
        };

        let memory_utilization = match (active, mapped) {
            (Some(act), Some(map)) if map > 0 => Some((act as f64 / map as f64) * 100.0),
            _ => None,
        };

        Self {
            available: true,
            allocated,
            active,
            mapped,
            retained,
            resident,
            metadata,
            arenas,
            memory_efficiency,
            memory_utilization,
        }
    }
}

impl fmt::Display for JemallocInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "JEMALLOC MEMORY STATISTICS")?;
        writeln!(f, "-------------------------")?;

        if !self.available {
            writeln!(f, "Jemalloc statistics: not available on this platform")?;
            writeln!(f)?;
            return Ok(());
        }

        macro_rules! display_stat {
            ($field:ident, $description:expr) => {
                match self.$field {
                    Some(value) => {
                        writeln!(
                            f,
                            "{}: {:.2} MB ({} bytes)",
                            $description,
                            value as f64 / (1024.0 * 1024.0),
                            value
                        )?;
                    }
                    None => {
                        writeln!(f, "{}: Error reading", $description)?;
                    }
                }
            };
        }

        display_stat!(allocated, "Allocated Memory");
        display_stat!(active, "Active Memory");
        display_stat!(mapped, "Mapped Memory");
        display_stat!(retained, "Retained Memory");
        display_stat!(resident, "Resident Memory");
        display_stat!(metadata, "Metadata Memory");

        if let Some(arenas) = self.arenas {
            writeln!(f, "Number of Arenas: {}", arenas)?;
        }

        if let Some(efficiency) = self.memory_efficiency {
            writeln!(
                f,
                "Memory Efficiency: {:.1}% (allocated/resident)",
                efficiency
            )?;
        }

        if let Some(utilization) = self.memory_utilization {
            writeln!(f, "Memory Utilization: {:.1}% (active/mapped)", utilization)?;
        }

        writeln!(f)
    }
}

/// CPU information and container details
struct CpuInfo {
    physical_cores: usize,
    cpu_model: String,
    average_frequency: Option<u64>,
    available_parallelism: Option<usize>,
    container_cpu: ContainerCpuInfo,
}

impl CpuInfo {
    /// Collect CPU information using sysinfo and improved cgroup detection
    async fn new(system: &System) -> Self {
        let cpus = system.cpus();
        let cpu_count = cpus.len();

        let (cpu_model, average_frequency) = if !cpus.is_empty() {
            // Get CPU brand/model from first CPU
            let cpu = &cpus[0];
            let model = cpu.brand().to_string();

            // Calculate average frequency
            let total_freq: u64 = cpus.iter().map(|cpu| cpu.frequency()).sum();
            let avg_freq = if total_freq > 0 {
                Some(total_freq / cpu_count as u64)
            } else {
                None
            };

            (model, avg_freq)
        } else {
            (String::new(), None)
        };

        // Thread parallelism info
        let available_parallelism = std::thread::available_parallelism().map(|p| p.get()).ok();

        // Improved container/cgroup CPU information
        let container_cpu = ContainerCpuInfo::new(system).await;

        Self {
            physical_cores: cpu_count,
            cpu_model,
            average_frequency,
            available_parallelism,
            container_cpu,
        }
    }
}

impl fmt::Display for CpuInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "CPU INFORMATION")?;
        writeln!(f, "---------------")?;
        writeln!(f, "Physical CPU cores: {}", self.physical_cores)?;

        if !self.cpu_model.is_empty() {
            writeln!(f, "CPU Model: {}", self.cpu_model)?;
        }

        if let Some(freq) = self.average_frequency {
            writeln!(f, "Average CPU Frequency: {} MHz", freq)?;
        }

        match self.available_parallelism {
            Some(parallelism) => {
                writeln!(f, "Available Parallelism: {} threads", parallelism)?;
            }
            None => {
                writeln!(f, "Available Parallelism: Unknown")?;
            }
        }

        writeln!(f, "{}", self.container_cpu)?;
        writeln!(f)
    }
}

/// Container/Kubernetes CPU information
struct ContainerCpuInfo {
    effective_cpu_count: u64,
    detection_method: String,
    system_cpu_count: u64,
    cpu_priority: Option<String>,
    k8s_cpu_request: Option<String>,
    k8s_cpu_limit: Option<String>,
}

impl ContainerCpuInfo {
    /// Enhanced container/Kubernetes CPU resource information with improved cgroup detection
    async fn new(system: &System) -> Self {
        let system_cpu_count = system.cpus().len() as u64;
        let (detection_method, effective_cpu_count) =
            Self::detect_effective_cpu_count(system_cpu_count).await;

        // Check CPU shares/weight (relative priority)
        let cpu_priority = Self::collect_cpu_priority_info().await;

        // Check for Kubernetes downward API environment variables
        let (k8s_cpu_request, k8s_cpu_limit) = Self::collect_k8s_cpu_info();

        Self {
            effective_cpu_count,
            detection_method,
            system_cpu_count,
            cpu_priority,
            k8s_cpu_request,
            k8s_cpu_limit,
        }
    }

    /// Detect effective CPU count considering cgroup limits (improved from fleet detector)
    #[cfg(not(target_os = "linux"))]
    async fn detect_effective_cpu_count(system_cpus: u64) -> (String, u64) {
        ("system".to_string(), system_cpus)
    }

    #[cfg(target_os = "linux")]
    async fn detect_effective_cpu_count(system_cpus: u64) -> (String, u64) {
        // Determine cgroup version first
        match tokio::fs::read_to_string("/proc/filesystems")
            .await
            .map(|fs| Self::detect_cgroup_version(&fs))
        {
            Ok(CGroupVersion::CGroup2) => {
                // cgroup v2: read from cpu.max
                match tokio::fs::read_to_string("/sys/fs/cgroup/cpu.max").await {
                    Ok(contents) => {
                        if contents.starts_with("max") {
                            ("system".to_string(), system_cpus)
                        } else {
                            match contents.split_once(' ') {
                                Some((quota, period)) => (
                                    "cgroup2".to_string(),
                                    Self::calculate_cpu_count_with_default(system_cpus, quota, period),
                                ),
                                None => ("system".to_string(), system_cpus),
                            }
                        }
                    }
                    Err(_) => ("system".to_string(), system_cpus),
                }
            }
            Ok(CGroupVersion::CGroup) => {
                // cgroup v1: read from separate quota and period files
                let quota = tokio::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
                    .await
                    .ok();
                let period = tokio::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
                    .await
                    .ok();

                match (quota, period) {
                    (Some(quota), Some(period)) => {
                        let quota = quota.trim();
                        let period = period.trim();
                        if quota == "-1" {
                            ("system".to_string(), system_cpus)
                        } else {
                            (
                                "cgroup".to_string(),
                                Self::calculate_cpu_count_with_default(system_cpus, quota, period),
                            )
                        }
                    }
                    _ => ("system".to_string(), system_cpus),
                }
            }
            _ => ("system".to_string(), system_cpus),
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

    /// Calculate CPU count from quota and period with fallback
    #[cfg(target_os = "linux")]
    fn calculate_cpu_count_with_default(default: u64, quota: &str, period: &str) -> u64 {
        match (quota.parse::<u64>(), period.parse::<u64>()) {
            (Ok(q), Ok(p)) if p > 0 => q / p,
            _ => default,
        }
    }

    /// Collect CPU priority information (shares/weight)
    async fn collect_cpu_priority_info() -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            // cgroup v1 shares
            if let Ok(cpu_shares) = tokio::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.shares").await {
                let shares = cpu_shares.trim();
                return Some(format!("CPU Shares (cgroup v1): {}", shares));
            }
            // cgroup v2 weight
            else if let Ok(cpu_weight) = tokio::fs::read_to_string("/sys/fs/cgroup/cpu.weight").await
            {
                let weight = cpu_weight.trim();
                return Some(format!("CPU Weight (cgroup v2): {}", weight));
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            // Return None for non-Linux platforms - the Display trait will handle this
        }

        None
    }

    /// Collect Kubernetes CPU resource information
    fn collect_k8s_cpu_info() -> (Option<String>, Option<String>) {
        let cpu_request = std::env::var("CPU_REQUEST").ok();
        let cpu_limit = std::env::var("CPU_LIMIT").ok();
        (cpu_request, cpu_limit)
    }
}

impl fmt::Display for ContainerCpuInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\nContainer/Kubernetes CPU Information:")?;
        writeln!(
            f,
            "Effective CPU Count: {} (detected via: {})",
            self.effective_cpu_count, self.detection_method
        )?;

        if self.effective_cpu_count != self.system_cpu_count {
            writeln!(
                f,
                "Note: System reports {} CPUs, but container is limited to {}",
                self.system_cpu_count, self.effective_cpu_count
            )?;
        }

        if let Some(ref priority) = self.cpu_priority {
            writeln!(f, "{}", priority)?;
        } else {
            writeln!(
                f,
                "CPU Priority Information: not available on this platform"
            )?;
        }

        if let Some(ref request) = self.k8s_cpu_request {
            writeln!(f, "CPU Request (K8s): {}", request)?;
        }
        if let Some(ref limit) = self.k8s_cpu_limit {
            writeln!(f, "CPU Limit (K8s): {}", limit)?;
        }

        Ok(())
    }
}

/// System load and CPU usage information
struct SystemLoadInfo {
    load_available: bool,
    load_one: f64,
    load_five: f64,
    load_fifteen: f64,
    cpu_count: usize,
    individual_cpu_usage: Vec<f32>,
    average_cpu_usage: f32,
}

impl SystemLoadInfo {
    /// Collect system load information (cross-platform)
    async fn new(system: &mut System) -> Self {
        // Use sysinfo's cross-platform load average if available
        let load_avg = System::load_average();
        let cpu_count = system.cpus().len();
        let load_available = load_avg.one.is_finite() && load_avg.one >= 0.0;

        // Take initial CPU measurement
        system.refresh_cpu_usage();

        // Wait for CPU usage calculation
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Take second measurement
        system.refresh_cpu_usage();

        let cpus = system.cpus();
        let mut individual_cpu_usage = Vec::new();
        let mut total_usage = 0.0f32;

        for cpu in cpus.iter() {
            let usage = cpu.cpu_usage();
            total_usage += usage;
            individual_cpu_usage.push(usage);
        }

        let average_cpu_usage = if !cpus.is_empty() {
            total_usage / cpus.len() as f32
        } else {
            0.0
        };

        Self {
            load_available,
            load_one: load_avg.one,
            load_five: load_avg.five,
            load_fifteen: load_avg.fifteen,
            cpu_count,
            individual_cpu_usage,
            average_cpu_usage,
        }
    }
}

impl fmt::Display for SystemLoadInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "SYSTEM LOAD")?;
        writeln!(f, "-----------")?;

        if self.load_available {
            writeln!(f, "Load Average (1min): {:.2}", self.load_one)?;
            writeln!(f, "Load Average (5min): {:.2}", self.load_five)?;
            writeln!(f, "Load Average (15min): {:.2}", self.load_fifteen)?;

            if self.cpu_count > 0 {
                let load_per_cpu_1min = self.load_one / self.cpu_count as f64;
                writeln!(
                    f,
                    "Load per CPU (1min): {:.2} ({:.1}% utilization)",
                    load_per_cpu_1min,
                    load_per_cpu_1min * 100.0
                )?;
            }

            writeln!(f, "Individual CPU Core Usage:")?;
            for (i, usage) in self.individual_cpu_usage.iter().enumerate() {
                writeln!(f, "  CPU {}: {:.1}%", i, usage)?;
            }
            writeln!(f, "Average CPU Usage: {:.1}%", self.average_cpu_usage)?;
        } else {
            writeln!(f, "Load Average: Not available on this platform")?;
            writeln!(f, "CPU Usage (per core):")?;
            for (i, usage) in self.individual_cpu_usage.iter().enumerate() {
                writeln!(f, "  CPU {}: {:.1}%", i, usage)?;
            }
            writeln!(
                f,
                "Total CPU Usage (average): {:.1}%",
                self.average_cpu_usage
            )?;
        }

        writeln!(f)
    }
}

/// Environment variables information
struct EnvironmentInfo {
    relevant_vars: Vec<(String, String)>,
}

impl EnvironmentInfo {
    /// Collect relevant environment variables
    ///
    /// SECURITY: Limited environment variable exposure
    /// Only expose specifically allowlisted environment variables that are safe for diagnostics.
    /// Never expose variables that might contain secrets, tokens, passwords, or API keys.
    fn new() -> Self {
        // SECURITY: Explicit allowlist of safe environment variables
        // Only Apollo-specific configuration variables are included
        // DO NOT add variables that might contain secrets (API keys, passwords, etc.)
        let relevant_vars = ["APOLLO_GRAPH_REF"];
        let mut collected_vars = Vec::new();

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
                collected_vars.push((var.to_string(), display_value));
            }
        }

        Self {
            relevant_vars: collected_vars,
        }
    }
}

impl fmt::Display for EnvironmentInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "RELEVANT ENVIRONMENT VARIABLES")?;
        writeln!(f, "------------------------------")?;

        if self.relevant_vars.is_empty() {
            writeln!(f, "No relevant Apollo environment variables set")?;
        } else {
            for (var, value) in &self.relevant_vars {
                writeln!(f, "{}: {}", var, value)?;
            }
        }

        writeln!(f)
    }
}

/// Collect system information
///
/// SECURITY WARNING: This function collects extensive system information that may be sensitive:
/// - Memory layout, CPU details, container environment
/// - Environment variables, filesystem paths, system architecture
/// - Should only be used in development/debugging environments with proper access controls
pub(crate) async fn collect() -> DiagnosticsResult<String> {
    let diagnostics = SystemDiagnostics::new().await;
    Ok(diagnostics.to_string())
}


/// CGroup version enumeration
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CGroupVersion {
    CGroup2,
    CGroup,
    None,
}

#[cfg(test)]
mod tests;
