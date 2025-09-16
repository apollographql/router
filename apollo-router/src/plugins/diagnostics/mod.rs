//! Diagnostics plugin
//!
//! Provides web endpoints for runtime diagnostics including memory profiling.
//!
//! This plugin exposes endpoints for:
//! - Memory profiling control (cross-platform with graceful degradation)
//! - Heap dump generation (Linux-only, requires jemalloc)
//! - Profiling status monitoring
//! - Diagnostic data export
//!
//! **Platform Support**: This plugin is available on all platforms.
//! Heap dump functionality is only available on Linux due to jemalloc requirements.

use std::sync::Arc;

use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;

use crate::Endpoint;
use crate::configuration::ListenAddr;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;

mod archive_utils;
mod constants;
mod export;
mod html_generator;
mod js_resources;
mod memory;
mod response_builder;
mod security;
mod service;
pub(crate) mod system_info;

#[cfg(test)]
mod tests;

use service::DiagnosticsService;

/// Simplified error types for the diagnostics plugin
#[derive(Debug, thiserror::Error)]
pub(crate) enum DiagnosticsError {
    /// I/O operation errors
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// HTTP response errors
    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    /// JSON serialization/deserialization errors
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Jemalloc/memory profiling errors
    #[error("Memory profiling error: {0}")]
    #[cfg_attr(
        not(all(target_family = "unix", feature = "global-allocator")),
        allow(dead_code)
    )]
    Memory(String),

    /// Internal errors (catch-all)
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type alias for diagnostics operations
pub(crate) type DiagnosticsResult<T> = Result<T, DiagnosticsError>;

impl From<String> for DiagnosticsError {
    fn from(error: String) -> Self {
        DiagnosticsError::Internal(error)
    }
}

/// Configuration for the diagnostics plugin
///
/// **Platform Requirements**: This plugin is supported on all platforms.
/// Heap dump functionality is only available on Linux platforms due to
/// jemalloc requirements. Other diagnostic features work across platforms.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Config {
    /// Enable the diagnostics plugin
    pub(crate) enabled: bool,

    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:8089
    pub(crate) listen: ListenAddr,

    /// Directory path for memory dump files
    /// Defaults to "/tmp/router-diagnostics" on Unix, or temp directory on other platforms
    ///
    /// This directory will be created automatically if it doesn't exist.
    /// Note: Memory dumps are only generated on Linux platforms.
    pub(crate) output_directory: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // SECURITY: Plugin disabled by default to prevent accidental exposure
            // Diagnostics endpoints expose sensitive information and should only be enabled
            // during development/debugging with proper network isolation
            enabled: false,

            // SECURITY: Bind to localhost only by default to prevent network exposure
            // Using 127.0.0.1 instead of 0.0.0.0 ensures the diagnostic endpoints
            // are only accessible from the local machine, not from the network
            listen: constants::network::default_listen_addr().into(),

            output_directory: constants::files::DEFAULT_OUTPUT_DIR_UNIX.to_string(),
        }
    }
}

/// The diagnostics plugin
#[derive(Debug, Clone)]
struct DiagnosticsPlugin {
    config: Config,
    router_config: Arc<String>,
    supergraph_schema: Arc<String>,
}

#[async_trait::async_trait]
impl Plugin for DiagnosticsPlugin {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            config: init.config,
            supergraph_schema: init.supergraph_sdl,
            // Many tests do not supply config, so just default it.
            router_config: init
                .original_config_yaml
                .unwrap_or(Arc::new("".to_string())),
        })
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        if !self.config.enabled {
            return MultiMap::new();
        }

        let mut map = MultiMap::new();
        let exporter = export::Exporter::new(
            self.config.clone(),
            self.supergraph_schema.clone(),
            self.router_config.clone(),
        );

        let wildcard_endpoint = Endpoint::from_router_service(
            "/diagnostics/{*wildcard}".to_string(),
            DiagnosticsService::new(
                self.config.output_directory.clone(),
                exporter.clone(),
                self.router_config.clone(),
                self.supergraph_schema.clone(),
            )
            .boxed(),
        );

        let root_endpoint = Endpoint::from_router_service(
            "/diagnostics".to_string(),
            DiagnosticsService::new(
                self.config.output_directory.clone(),
                exporter,
                self.router_config.clone(),
                self.supergraph_schema.clone(),
            )
            .boxed(),
        );

        tracing::info!(
            "Diagnostics endpoints at {}/diagnostics",
            self.config.listen
        );
        map.insert(self.config.listen.clone(), wildcard_endpoint);
        map.insert(self.config.listen.clone(), root_endpoint);
        map
    }
}

register_plugin!("apollo", "experimental_diagnostics", DiagnosticsPlugin);
