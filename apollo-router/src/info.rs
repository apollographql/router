//! Router system info for support tickets and diagnostics.
//!
//! Provides a single source of truth for static router metadata (version, OS, arch,
//! startup options, config/supergraph path+hash, set env var names) and shared
//! logic for Router-relevant environment variables.

use once_cell::sync::OnceCell;
use serde::Serialize;

/// Router-relevant environment variable names (from Opt and other Router usage).
/// No broad prefixes; only vars the Router actually reads.
const ROUTER_RELEVANT_ENV_VARS: &[&str] = &[
    "APOLLO_GRAPH_ARTIFACT_REFERENCE",
    "APOLLO_GRAPH_REF",
    "APOLLO_KEY",
    "APOLLO_KEY_PATH",
    "APOLLO_ROUTER_CONFIG_PATH",
    "APOLLO_ROUTER_DEV",
    "APOLLO_ROUTER_HOT_RELOAD",
    "APOLLO_ROUTER_LICENSE",
    "APOLLO_ROUTER_LICENSE_PATH",
    "APOLLO_ROUTER_LISTEN_ADDRESS",
    "APOLLO_ROUTER_LOG",
    "APOLLO_ROUTER_SUPERGRAPH_PATH",
    "APOLLO_ROUTER_SUPERGRAPH_URLS",
    "APOLLO_TELEMETRY_DISABLED",
    "APOLLO_UPLINK_ENDPOINTS",
    "APOLLO_UPLINK_TIMEOUT",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "RUST_LOG",
];

/// Returns sorted list of **set** Router-relevant env var names (values never included).
pub(crate) fn set_relevant_env_var_names() -> Vec<String> {
    let mut names: Vec<String> = ROUTER_RELEVANT_ENV_VARS
        .iter()
        .filter(|name| std::env::var(name).is_ok())
        .map(|s| (*s).to_string())
        .collect();
    names.sort();
    names
}

/// Redacted startup options for display (secrets shown as "set" only).
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct StartupOptions {
    pub(crate) log_level: Option<String>,
    pub(crate) hot_reload: bool,
    pub(crate) dev: bool,
    pub(crate) listen_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supergraph_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supergraph_urls: Option<String>,
    pub(crate) apollo_key_set: bool,
    pub(crate) apollo_graph_ref_set: bool,
    pub(crate) apollo_router_license_set: bool,
    pub(crate) apollo_router_license_path_set: bool,
    pub(crate) graph_artifact_reference_set: bool,
    pub(crate) anonymous_telemetry_disabled: bool,
}

/// Static router system info: version, OS, arch, build, options, config/supergraph path+hash, env names.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouterSystemInfo {
    pub(crate) version: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) target_family: String,
    /// Build type (e.g. "Release (optimized)" or "Debug (with debug assertions)").
    pub(crate) build_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rust_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) build_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target_triple: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) optimization_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supergraph_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supergraph_hash: Option<String>,
    pub(crate) startup_options: StartupOptions,
    pub(crate) set_env_var_names: Vec<String>,
}

static ROUTER_SYSTEM_INFO: OnceCell<RouterSystemInfo> = OnceCell::new();

/// Set the global router system info (called once at startup by the executable).
pub(crate) fn set_router_system_info(info: RouterSystemInfo) {
    let _ = ROUTER_SYSTEM_INFO.set(info);
}

/// Get the global router system info if it has been set.
pub(crate) fn get_router_system_info() -> Option<&'static RouterSystemInfo> {
    ROUTER_SYSTEM_INFO.get()
}

impl RouterSystemInfo {
    /// Format for CLI output (path + hash only for config/supergraph; no file contents).
    pub(crate) fn format_for_cli(&self) -> String {
        let mut out = String::new();
        out.push_str("Router version: ");
        out.push_str(&self.version);
        out.push('\n');
        out.push_str("OS / architecture: ");
        out.push_str(&self.os);
        out.push_str(" / ");
        out.push_str(&self.arch);
        out.push_str(" (");
        out.push_str(&self.target_family);
        out.push_str(")\n");
        out.push_str("Build type: ");
        out.push_str(&self.build_type);
        out.push('\n');
        if let Some(ref rv) = self.rust_version {
            out.push_str("Rust version: ");
            out.push_str(rv);
            out.push('\n');
        }
        if let Some(ref p) = self.build_profile {
            out.push_str("Build profile: ");
            out.push_str(p);
            out.push('\n');
        }
        if let Some(ref t) = self.target_triple {
            out.push_str("Target triple: ");
            out.push_str(t);
            out.push('\n');
        }
        if let Some(ref o) = self.optimization_level {
            out.push_str("Optimization level: ");
            out.push_str(o);
            out.push('\n');
        }
        out.push_str("Startup options:\n");
        if let Some(ref l) = self.startup_options.log_level {
            out.push_str(&format!("  --log {}\n", l));
        }
        if self.startup_options.hot_reload {
            out.push_str("  --hot-reload\n");
        }
        if self.startup_options.dev {
            out.push_str("  --dev\n");
        }
        if let Some(ref a) = self.startup_options.listen_address {
            out.push_str(&format!("  --listen {}\n", a));
        }
        if self.startup_options.config_path.is_some() {
            out.push_str("  --config (set)\n");
        }
        if self.startup_options.supergraph_path.is_some() {
            out.push_str("  --supergraph (set)\n");
        }
        if self.startup_options.supergraph_urls.is_some() {
            out.push_str("  --supergraph-urls (set)\n");
        }
        if self.startup_options.apollo_key_set {
            out.push_str("  APOLLO_KEY (set)\n");
        }
        if self.startup_options.apollo_graph_ref_set {
            out.push_str("  APOLLO_GRAPH_REF (set)\n");
        }
        if self.startup_options.apollo_router_license_set {
            out.push_str("  APOLLO_ROUTER_LICENSE (set)\n");
        }
        if self.startup_options.apollo_router_license_path_set {
            out.push_str("  APOLLO_ROUTER_LICENSE_PATH (set)\n");
        }
        if self.startup_options.graph_artifact_reference_set {
            out.push_str("  APOLLO_GRAPH_ARTIFACT_REFERENCE (set)\n");
        }
        if self.startup_options.anonymous_telemetry_disabled {
            out.push_str("  APOLLO_TELEMETRY_DISABLED (set)\n");
        }
        out.push_str("Config file: ");
        match (&self.config_path, &self.config_hash) {
            (Some(p), Some(h)) => out.push_str(&format!("path={} hash={}\n", p, h)),
            (Some(p), None) => out.push_str(&format!("path={} (hash not available)\n", p)),
            (None, _) => out.push_str("(default or not from file)\n"),
        }
        out.push_str("Supergraph: ");
        match (&self.supergraph_source, &self.supergraph_hash) {
            (Some(s), Some(h)) => out.push_str(&format!("source={} hash={}\n", s, h)),
            (Some(s), None) => out.push_str(&format!("source={} (hash not available)\n", s)),
            (None, _) => out.push_str("(not set)\n"),
        }
        out.push_str("Environment variables set: ");
        if self.set_env_var_names.is_empty() {
            out.push_str("(none)\n");
        } else {
            out.push_str(&self.set_env_var_names.join(", "));
            out.push('\n');
        }
        out
    }

    /// Compact format for boot log (path + hash only for config/supergraph).
    pub(crate) fn format_for_boot_log(&self) -> String {
        let config = self.config_path.as_deref().unwrap_or("default");
        let config_hash = self.config_hash.as_deref().unwrap_or("-");
        let supergraph = self.supergraph_source.as_deref().unwrap_or("-");
        let schema_hash = self.supergraph_hash.as_deref().unwrap_or("-");
        format!(
            "router info: version={} os={} arch={} config={} config_hash={} supergraph={} schema_hash={} env_count={}",
            self.version,
            self.os,
            self.arch,
            config,
            config_hash,
            supergraph,
            schema_hash,
            self.set_env_var_names.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_relevant_env_var_names_sorted_and_set_only() {
        let names = set_relevant_env_var_names();
        let sorted = names.clone();
        let mut sorted_expected = sorted.clone();
        sorted_expected.sort();
        assert_eq!(sorted, sorted_expected, "names should be sorted");
    }
}
