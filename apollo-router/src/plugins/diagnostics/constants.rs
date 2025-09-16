//! Constants for the diagnostics plugin
//!
//! This module centralizes constants used throughout the diagnostics plugin.
//! Only includes constants that are actually used to avoid code bloat.

use std::net::SocketAddr;
use std::str::FromStr;

/// Default network configuration
pub(crate) mod network {
    use super::*;

    /// Default localhost IP address for security (prevents network exposure)
    const LOCALHOST_IP: &str = "127.0.0.1";

    /// Default port for diagnostics endpoints
    const DEFAULT_PORT: u16 = 8089;

    /// Default diagnostics listen address (localhost only for security)
    pub(crate) fn default_listen_addr() -> SocketAddr {
        SocketAddr::from_str(&format!("{}:{}", LOCALHOST_IP, DEFAULT_PORT))
            .expect("Valid default diagnostics listen address")
    }
}

/// File and directory constants
pub(crate) mod files {
    /// Default output directory for diagnostics files on Unix systems
    pub(crate) const DEFAULT_OUTPUT_DIR_UNIX: &str = "/tmp/router-diagnostics";
}

/// Route path constants
pub(crate) mod routes {
    /// Base diagnostics route
    pub(crate) const BASE: &str = "/diagnostics";

    /// Memory-related routes
    pub(crate) mod memory {
        pub(crate) const STATUS: &str = "memory/status";
        pub(crate) const DUMPS: &str = "memory/dumps";
        pub(crate) const START: &str = "memory/start";
        pub(crate) const STOP: &str = "memory/stop";
        pub(crate) const DUMP: &str = "memory/dump";
        pub(crate) const DUMPS_PREFIX: &str = "memory/dumps/";
    }

    /// Export and data routes
    pub(crate) const EXPORT: &str = "export";
    pub(crate) const SYSTEM_INFO: &str = "system_info.txt";
    pub(crate) const ROUTER_CONFIG: &str = "router_config.yaml";
    pub(crate) const SUPERGRAPH_SCHEMA: &str = "supergraph.graphql";

    /// JavaScript resource files
    pub(crate) mod js_resources {
        pub(crate) const BACKTRACE_PROCESSOR: &str = "backtrace-processor.js";
        pub(crate) const VIZ_JS_INTEGRATION: &str = "viz-js-integration.js";
        pub(crate) const FLAMEGRAPH_RENDERER: &str = "flamegraph-renderer.js";
        pub(crate) const CALLGRAPH_SVG_RENDERER: &str = "callgraph-svg-renderer.js";
        pub(crate) const DATA_ACCESS: &str = "data-access.js";
        pub(crate) const MAIN: &str = "main.js";
        pub(crate) const CUSTOM_ELEMENTS: &str = "custom_elements.js";
    }
}

/// Error messages and descriptions
pub(crate) mod messages {
    /// Error messages for common scenarios
    pub(crate) mod errors {
        pub(crate) const NOT_FOUND: &str = "Endpoint not found. Available: GET /, GET export, GET memory/status, GET memory/dumps, DELETE memory/dumps, POST memory/start, POST memory/stop, POST memory/dump, GET memory/dumps/{filename}, DELETE memory/dumps/{filename}";
        pub(crate) const INTERNAL_ERROR: &str = "Internal server error";
    }
}
