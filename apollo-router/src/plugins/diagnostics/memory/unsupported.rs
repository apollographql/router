//! Memory profiling stub implementation for unsupported platforms
//!
//! This module provides a stub implementation of the memory profiling service
//! for platforms that don't support jemalloc heap profiling. It ensures API
//! compatibility across all platforms while gracefully returning "not supported"
//! messages instead of failing at compile time.
//!
//! ## Purpose
//!
//! - **Graceful degradation**: Allow diagnostics plugin to work on all platforms
//! - **API compatibility**: Same method signatures as the supported implementation
//! - **Clear messaging**: Return helpful error messages explaining feature unavailability
//!
//! ## Platforms
//!
//! This stub is compiled on:
//! - Windows (jemalloc not available)
//! - Non-Unix systems
//! - Unix systems without `global-allocator` feature flag
//!
//! ## Behavior
//!
//! All memory profiling operations return JSON responses with `status: "not_supported"`
//! and appropriate error messages. Archive operations add empty placeholders.
//!
//! ## See Also
//!
//! - [`super::supported`] - Full implementation for Unix + global-allocator platforms

use axum::body::Body;
use http::Response;
use http::StatusCode;
use serde_json::json;

use crate::plugins::diagnostics::DiagnosticsResult;
use crate::plugins::diagnostics::response_builder::CacheControl;
use crate::plugins::diagnostics::response_builder::ResponseBuilder;

/// Memory profiling service stub for unsupported platforms
#[derive(Clone)]
pub(crate) struct MemoryService {
    #[allow(dead_code)]
    pub output_directory: String,
}

impl MemoryService {
    pub(crate) fn new(output_directory: String) -> Self {
        Self { output_directory }
    }

    /// Helper to build JSON responses
    fn json_response(
        &self,
        status: StatusCode,
        data: serde_json::Value,
    ) -> DiagnosticsResult<Response<Body>> {
        ResponseBuilder::json_response(status, &data, CacheControl::NoCache)
    }

    /// Helper for unsupported platform response
    fn unsupported_platform_response(&self) -> DiagnosticsResult<Response<Body>> {
        let response = json!({
            "status": "not_supported",
            "message": format!("Memory profiling not supported: requires Linux platform with jemalloc global allocator enabled (current: {})", std::env::consts::OS),
            "platform": std::env::consts::OS
        });
        self.json_response(StatusCode::NOT_IMPLEMENTED, response)
    }

    /// Handle GET /diagnostics/memory/status
    pub(crate) async fn handle_status(&self) -> DiagnosticsResult<Response<Body>> {
        let status = json!({
            "profiling_active": false,
            "status": "not_available",
            "platform": std::env::consts::OS,
            "heap_dumps_available": false,
            "message": "Memory profiling requires Linux platform with jemalloc global allocator enabled"
        });

        self.json_response(StatusCode::OK, status)
    }

    /// Handle POST /diagnostics/memory/start
    pub(crate) async fn handle_start(&self) -> DiagnosticsResult<Response<Body>> {
        self.unsupported_platform_response()
    }

    /// Handle POST /diagnostics/memory/stop
    pub(crate) async fn handle_stop(&self) -> DiagnosticsResult<Response<Body>> {
        self.unsupported_platform_response()
    }

    /// Handle POST /diagnostics/memory/dump
    pub(crate) async fn handle_dump(&self) -> DiagnosticsResult<Response<Body>> {
        tracing::info!("Memory dump requested");

        let response = json!({
            "status": "not_supported",
            "message": format!("Heap dumps not supported: requires Linux platform with jemalloc global allocator enabled (current: {})", std::env::consts::OS),
            "platform": std::env::consts::OS
        });

        self.json_response(StatusCode::NOT_IMPLEMENTED, response)
    }

    /// Adds memory diagnostic data to an existing tar archive (stub implementation)
    pub(crate) async fn add_to_archive<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        _output_directory: &str,
    ) -> DiagnosticsResult<()> {
        tracing::warn!("Memory diagnostic archiving not supported on this platform");

        // Add README.txt explaining why memory profiling is not available
        // The ArchiveUtils will automatically create the necessary parent directories
        let readme_content = format!(
            "MEMORY PROFILING NOT AVAILABLE\n\
            ==============================\n\n\
            Memory profiling with heap dumps is only available on Linux platforms\n\
            with jemalloc global allocator enabled.\n\n\
            Current platform: {}\n\n\
            To enable memory profiling:\n\
            1. Use a Linux platform (Ubuntu, CentOS, etc.)\n\
            2. Ensure jemalloc is compiled with the 'global-allocator' feature\n\
            3. Build the router with memory profiling support\n\n\
            For more information, see the Apollo Router documentation.\n",
            std::env::consts::OS
        );

        use crate::plugins::diagnostics::archive_utils::ArchiveUtils;
        ArchiveUtils::add_text_file(tar, "memory/README.txt", &readme_content)
            .await
            .map_err(|e| format!("Failed to add README.txt: {}", e))?;

        Ok(())
    }

    /// Handle GET /diagnostics/memory/dumps - List dumps (unsupported)
    pub(crate) async fn handle_list_dumps(&self) -> DiagnosticsResult<Response<Body>> {
        self.json_response(StatusCode::OK, serde_json::json!([]))
    }

    /// Handle GET /diagnostics/memory/dumps/{filename} - Download dump (unsupported)
    pub(crate) async fn handle_download_dump(
        &self,
        _filename: &str,
    ) -> DiagnosticsResult<Response<Body>> {
        self.unsupported_platform_response()
    }

    /// Handle DELETE /diagnostics/memory/dumps/{filename} - Delete dump (unsupported)
    pub(crate) async fn handle_delete_dump(
        &self,
        _filename: &str,
    ) -> DiagnosticsResult<Response<Body>> {
        self.unsupported_platform_response()
    }

    /// Handle DELETE /diagnostics/memory/dumps - Clear all dumps (unsupported)
    pub(crate) async fn handle_clear_all_dumps(&self) -> DiagnosticsResult<Response<Body>> {
        self.unsupported_platform_response()
    }
}
