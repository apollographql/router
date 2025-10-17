//! Constants for the diagnostics plugin
//!
//! This module centralizes constants used throughout the diagnostics plugin.

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
        SocketAddr::from_str(&format!("{LOCALHOST_IP}:{DEFAULT_PORT}"))
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

    /// CSS resource files
    pub(crate) mod css_resources {
        pub(crate) const STYLES: &str = "styles.css";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_listen_addr_is_localhost() {
        // SECURITY: Diagnostics must only bind to localhost to prevent network exposure
        let addr = network::default_listen_addr();

        assert!(
            addr.ip().is_loopback(),
            "Diagnostics endpoint MUST bind to localhost (127.0.0.1) for security, got: {}",
            addr.ip()
        );
    }

    #[test]
    fn test_default_output_dir_is_absolute() {
        assert!(
            files::DEFAULT_OUTPUT_DIR_UNIX.starts_with('/'),
            "Output directory must be an absolute path for consistency"
        );
    }

    #[test]
    fn test_all_resource_paths_are_unique() {
        use std::collections::HashSet;

        let all_resources = [
            routes::js_resources::BACKTRACE_PROCESSOR,
            routes::js_resources::VIZ_JS_INTEGRATION,
            routes::js_resources::FLAMEGRAPH_RENDERER,
            routes::js_resources::CALLGRAPH_SVG_RENDERER,
            routes::js_resources::DATA_ACCESS,
            routes::js_resources::MAIN,
            routes::js_resources::CUSTOM_ELEMENTS,
            routes::css_resources::STYLES,
        ];

        let unique: HashSet<_> = all_resources.iter().collect();
        assert_eq!(
            unique.len(),
            all_resources.len(),
            "All resource paths must be unique"
        );
    }
}
