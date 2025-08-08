//! Export functionality for diagnostics plugin
//!
//! This module handles the creation of diagnostic archives containing data
//! from all diagnostic modules. It provides a comprehensive export system
//! that can be extended by additional diagnostic modules.
//!
//! **Platform Support**: This module is only available on Linux platforms.

use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::write::GzEncoder;
use flate2::Compression;
use http::StatusCode;
use serde_yaml;
use tower::BoxError;

use crate::services::router::Request;
use crate::services::router::Response;
use crate::services::router::body;
use super::Config;
use super::memory;

#[cfg(test)]
mod tests;

/// Export service responsible for creating comprehensive diagnostic archives
#[derive(Debug, Clone)]
pub(super) struct ExportService {
    config: Config,
    full_config: Option<serde_json::Value>,
}

impl ExportService {
    /// Create a new export service with the given configuration
    pub(super) fn new(config: Config, full_config: Option<serde_json::Value>) -> Self {
        Self { config, full_config }
    }

    /// Handle GET /diagnostics/export
    /// Creates a diagnostic archive by collecting data from all diagnostic modules
    #[cfg(target_os = "linux")]
    pub(super) async fn handle_export(&self, request: Request) -> Result<Response, BoxError> {
        tracing::info!("Diagnostic export requested");

        let config = self.config.clone();
        let full_config = self.full_config.clone();
        
        // Create archive in a blocking task
        let archive_data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, BoxError> {
            Self::create_archive(&config, &full_config)
        })
        .await
        .map_err(|e| BoxError::from(format!("Archive task failed: {}", e)))?
        .map_err(|e| BoxError::from(e.to_string()))?;

        // Generate filename with timestamp
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BoxError::from(e.to_string()))?
            .as_secs();
        
        let filename = format!("router-diagnostics-{}.tar.gz", timestamp);

        tracing::info!(
            "Diagnostic export created: {} bytes, filename: {}",
            archive_data.len(),
            filename
        );

        Ok(Response::http_response_builder()
            .response(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/gzip")
                    .header("content-disposition", format!("attachment; filename=\"{}\"", filename))
                    .header("content-length", archive_data.len())
                    .body(body::from_bytes(archive_data))?,
            )
            .context(request.context)
            .build()?)
    }

    /// Creates a diagnostic archive with contributions from all modules
    #[cfg(target_os = "linux")]
    fn create_archive(config: &Config, full_config: &Option<serde_json::Value>) -> Result<Vec<u8>, BoxError> {
        let mut archive_buffer = Vec::new();
        let cursor = Cursor::new(&mut archive_buffer);
        let encoder = GzEncoder::new(cursor, Compression::default());
        let mut tar = tar::Builder::new(encoder);

        // Create main manifest
        let manifest = Self::create_main_manifest(config)?;
        let mut manifest_header = tar::Header::new_gnu();
        manifest_header.set_path("manifest.txt")
            .map_err(|e| format!("Failed to set manifest path: {}", e))?;
        manifest_header.set_size(manifest.len() as u64);
        manifest_header.set_mode(0o644);
        manifest_header.set_cksum();
        
        tar.append(&manifest_header, Cursor::new(manifest))
            .map_err(|e| format!("Failed to add manifest: {}", e))?;

        // Add router.yaml configuration if available
        if let Some(full_config) = full_config {
            let router_yaml = serde_yaml::to_string(full_config)
                .map_err(|e| format!("Failed to serialize router config: {}", e))?;
            
            let mut config_header = tar::Header::new_gnu();
            config_header.set_path("router.yaml")
                .map_err(|e| format!("Failed to set router.yaml path: {}", e))?;
            config_header.set_size(router_yaml.len() as u64);
            config_header.set_mode(0o644);
            config_header.set_cksum();
            
            tar.append(&config_header, Cursor::new(router_yaml.as_bytes()))
                .map_err(|e| format!("Failed to add router.yaml: {}", e))?;
            
            tracing::info!("Added router.yaml configuration to archive");
        } else {
            tracing::warn!("No full configuration available, router.yaml not included in archive");
        }

        // Delegate to memory module to add its contents
        memory::MemoryService::add_to_archive(&mut tar, &config.output_directory)?;

        // Add router binary (skip in tests)
        Self::add_router_binary(&mut tar)?;

        // Finish the tar archive
        let encoder = tar.into_inner()
            .map_err(|e| format!("Failed to finalize tar: {}", e))?;
        let _cursor = encoder.finish()
            .map_err(|e| format!("Failed to finalize gzip: {}", e))?;

        tracing::info!("Created comprehensive diagnostic archive: {} bytes", archive_buffer.len());

        Ok(archive_buffer)
    }

    /// Add the router binary to the archive
    #[cfg(target_os = "linux")]
    fn add_router_binary<W: std::io::Write>(tar: &mut tar::Builder<W>) -> Result<(), BoxError> {
        if !cfg!(test) {
            if let Ok(current_exe) = std::env::current_exe() {
                tracing::info!("Adding router binary: {:?}", current_exe);
                tar.append_path_with_name(&current_exe, "router-binary")
                    .map_err(|e| format!("Failed to add router binary: {}", e))?;
            }
        } else {
            // Add placeholder in test mode
            let placeholder = b"Router binary skipped in test mode";
            let mut binary_header = tar::Header::new_gnu();
            binary_header.set_path("router-binary.txt")
                .map_err(|e| format!("Failed to set binary placeholder path: {}", e))?;
            binary_header.set_size(placeholder.len() as u64);
            binary_header.set_mode(0o644);
            binary_header.set_cksum();
            
            tar.append(&binary_header, Cursor::new(placeholder))
                .map_err(|e| format!("Failed to add binary placeholder: {}", e))?;
        }
        
        Ok(())
    }

    /// Creates the main manifest for the diagnostic archive
    #[cfg(target_os = "linux")]
    fn create_main_manifest(config: &Config) -> Result<Vec<u8>, BoxError> {
        let mut manifest = String::new();
        manifest.push_str("APOLLO ROUTER DIAGNOSTIC ARCHIVE\n");
        manifest.push_str("================================\n\n");
        
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        
        manifest.push_str(&format!("Generated: {}\n", timestamp));
        manifest.push_str(&format!("Router Version: {}\n", env!("CARGO_PKG_VERSION")));
        manifest.push_str(&format!("Platform: {}\n", std::env::consts::OS));
        manifest.push_str(&format!("Memory Output Directory: {}\n\n", config.output_directory));

        manifest.push_str("Archive Contents:\n");
        manifest.push_str("  - manifest.txt (this file)\n");
        manifest.push_str("  - router.yaml (full router configuration)\n");
        manifest.push_str("  - memory/ (memory profiling data)\n");
        if cfg!(test) {
            manifest.push_str("  - router-binary.txt (placeholder in test mode)\n");
        } else {
            manifest.push_str("  - router-binary (current router executable)\n");
        }

        manifest.push_str("\nModule Information:\n");
        manifest.push_str("  - Memory Profiling: Enabled (Linux)\n");
        manifest.push_str("    * Heap dump files in memory/ directory\n");
        manifest.push_str("    * Uses jemalloc profiling\n");

        Ok(manifest.into_bytes())
    }

    /// Get the configuration for testing purposes
    #[cfg(test)]
    pub(super) fn config(&self) -> &Config {
        &self.config
    }
}