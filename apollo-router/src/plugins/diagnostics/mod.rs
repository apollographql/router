//! Diagnostics plugin
//!
//! Provides web endpoints for runtime diagnostics including memory profiling.
//!
//! This plugin exposes endpoints for:
//! - Memory profiling control (jemalloc)
//! - Heap dump generation
//! - Profiling status monitoring
//!
//! All endpoints require authentication via a shared secret.
//!
//! **Platform Support**: This plugin is only available on Linux platforms due to
//! its dependency on specific jemalloc features.

use std::net::SocketAddr;
use std::str::FromStr;

use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;

use crate::Endpoint;
use crate::configuration::ListenAddr;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;
use crate::services::router::Request;
use crate::services::router::Response;

#[cfg(target_os = "linux")]
mod memory;
#[cfg(target_os = "linux")]
mod service;
#[cfg(target_os = "linux")]
mod export;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
use service::DiagnosticsService;

/// Configuration for the diagnostics plugin
/// 
/// **Platform Requirements**: This plugin is only supported on Linux platforms
/// due to its dependency on Linux-specific jemalloc features. Attempting to
/// enable this plugin on other platforms will result in a startup error.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Config {
    /// Enable the diagnostics plugin
    /// 
    /// **Note**: Only supported on Linux platforms. Enabling on other platforms
    /// will cause router startup to fail with an error message.
    pub(crate) enabled: bool,

    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:8089
    pub(crate) listen: ListenAddr,

    /// Shared secret for authenticating requests
    /// Required when enabled is true
    pub(crate) shared_secret: String,

    /// Directory path for memory dump files
    /// Defaults to "/tmp/router-diagnostics"
    /// 
    /// This directory will be created automatically if it doesn't exist.
    pub(crate) output_directory: String,
}

fn default_diagnostics_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:8089").unwrap().into()
}

fn default_diagnostics_enabled() -> bool {
    false
}

fn default_output_directory() -> String {
    "/tmp/router-diagnostics".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: default_diagnostics_enabled(),
            listen: default_diagnostics_listen(),
            shared_secret: String::new(),
            output_directory: default_output_directory(),
        }
    }
}

/// The diagnostics plugin
#[derive(Debug, Clone)]
struct Diagnostics {
    config: Config,
    full_config: Option<serde_json::Value>,
}

#[async_trait::async_trait]
impl PluginPrivate for Diagnostics {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        // Validate configuration when enabled
        if init.config.enabled {
            // Check platform compatibility first
            #[cfg(not(target_os = "linux"))]
            {
                tracing::error!(
                    "The diagnostics plugin is only supported on Linux platforms. \
                    Current platform: {}. Please disable the diagnostics plugin in your configuration.",
                    std::env::consts::OS
                );
                return Err(format!(
                    "Diagnostics plugin is not supported on this platform ({}). \
                    This plugin requires Linux-specific jemalloc features. \
                    Please set 'experimental_diagnostics.enabled: false' in your router configuration.",
                    std::env::consts::OS
                ).into());
            }

            if init.config.shared_secret.is_empty() {
                return Err("diagnostics plugin requires a shared_secret when enabled".into());
            }

            #[cfg(target_os = "linux")]
            tracing::info!(
                "Diagnostics endpoints exposed at {}/diagnostics/* (Linux platform detected)",
                init.config.listen
            );
        }

        Ok(Self {
            config: init.config,
            full_config: init.full_config,
        })
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();

        tracing::debug!("web_endpoints() called, enabled: {}", self.config.enabled);

        if self.config.enabled {
            // On Linux, create the actual service
            #[cfg(target_os = "linux")]
            {
                let shared_secret = self.config.shared_secret.clone();
                let output_directory = self.config.output_directory.clone();
                let diagnostics_plugin = std::sync::Arc::new(self.clone());

                // Register wildcard path for diagnostic endpoints
                let wildcard_endpoint = Endpoint::from_router_service(
                    "/diagnostics/{*wildcard}".to_string(),
                    DiagnosticsService::new(shared_secret, output_directory, diagnostics_plugin).boxed(),
                );

                tracing::info!(
                    "Registering diagnostics endpoints at {}: /diagnostics/{{*wildcard}}", 
                    self.config.listen
                );
                map.insert(self.config.listen.clone(), wildcard_endpoint);
            }

            // On non-Linux platforms, this should never be reached due to initialization check,
            // but add a safeguard just in case
            #[cfg(not(target_os = "linux"))]
            {
                tracing::error!(
                    "Diagnostics plugin enabled on unsupported platform {}. No endpoints registered.",
                    std::env::consts::OS
                );
            }
        } else {
            tracing::info!("Diagnostics plugin disabled, not registering endpoints");
        }

        tracing::info!("web_endpoints() returning {} endpoints", map.len());
        map
    }

}

impl Diagnostics {
    /// Handle GET /diagnostics/export
    /// Creates a comprehensive diagnostic archive by collecting data from all diagnostic modules
    #[cfg(target_os = "linux")]
    pub(super) async fn handle_export(&self, request: Request) -> Result<Response, BoxError> {
        let export_service = export::ExportService::new(self.config.clone(), self.full_config.clone());
        export_service.handle_export(request).await
    }
}

register_private_plugin!("apollo", "experimental_diagnostics", Diagnostics);
