//! Memory profiling stub implementation for unsupported platforms

use http::StatusCode;
use serde_json::json;

use crate::plugins::diagnostics::DiagnosticsResult;
use crate::plugins::diagnostics::response_builder::CacheControl;
use crate::plugins::diagnostics::response_builder::ResponseBuilder;
use crate::services::router::Request;
use crate::services::router::Response;

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
        request: Request,
    ) -> DiagnosticsResult<Response> {
        ResponseBuilder::json_response(
            status,
            &data,
            CacheControl::NoCache,
            request.context.clone(),
        )
    }

    /// Helper for unsupported platform response
    fn unsupported_platform_response(&self, request: Request) -> DiagnosticsResult<Response> {
        let response = json!({
            "status": "not_supported",
            "message": format!("Memory profiling not supported: requires Linux platform with jemalloc global allocator enabled (current: {})", std::env::consts::OS),
            "platform": std::env::consts::OS
        });
        self.json_response(StatusCode::NOT_IMPLEMENTED, response, request)
    }

    /// Handle GET /diagnostics/memory/status
    pub(crate) async fn handle_status(&self, request: Request) -> DiagnosticsResult<Response> {
        let status = json!({
            "profiling_active": false,
            "status": "not_available",
            "platform": std::env::consts::OS,
            "heap_dumps_available": false,
            "message": "Memory profiling requires Linux platform with jemalloc global allocator enabled"
        });

        self.json_response(StatusCode::OK, status, request)
    }

    /// Handle POST /diagnostics/memory/start
    pub(crate) async fn handle_start(&self, request: Request) -> DiagnosticsResult<Response> {
        self.unsupported_platform_response(request)
    }

    /// Handle POST /diagnostics/memory/stop
    pub(crate) async fn handle_stop(&self, request: Request) -> DiagnosticsResult<Response> {
        self.unsupported_platform_response(request)
    }

    /// Handle POST /diagnostics/memory/dump
    pub(crate) async fn handle_dump(&self, request: Request) -> DiagnosticsResult<Response> {
        tracing::info!("Memory dump requested");

        let response = json!({
            "status": "not_supported",
            "message": format!("Heap dumps not supported: requires Linux platform with jemalloc global allocator enabled (current: {})", std::env::consts::OS),
            "platform": std::env::consts::OS
        });

        self.json_response(StatusCode::NOT_IMPLEMENTED, response, request)
    }

    /// Adds memory diagnostic data to an existing tar archive (stub implementation)
    pub(crate) async fn add_to_archive<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        _output_directory: &str,
    ) -> DiagnosticsResult<()> {
        tracing::warn!("Memory diagnostic archiving not supported on this platform");

        // Create empty memory directory in archive
        let mut header = tokio_tar::Header::new_gnu();
        header
            .set_path("memory/")
            .map_err(|e| format!("Failed to set memory directory path: {}", e))?;
        header.set_entry_type(tokio_tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();

        let empty: &[u8] = &[];
        tar.append(&header, empty)
            .await
            .map_err(|e| format!("Failed to add memory directory: {}", e))?;

        Ok(())
    }

    /// Handle GET /diagnostics/memory/dumps - List dumps (unsupported)
    pub(crate) async fn handle_list_dumps(&self, request: Request) -> DiagnosticsResult<Response> {
        self.json_response(StatusCode::OK, serde_json::json!([]), request)
    }

    /// Handle GET /diagnostics/memory/dumps/{filename} - Download dump (unsupported)
    pub(crate) async fn handle_download_dump(
        &self,
        request: Request,
        _filename: &str,
    ) -> DiagnosticsResult<Response> {
        self.unsupported_platform_response(request)
    }

    /// Handle DELETE /diagnostics/memory/dumps/{filename} - Delete dump (unsupported)
    pub(crate) async fn handle_delete_dump(
        &self,
        request: Request,
        _filename: &str,
    ) -> DiagnosticsResult<Response> {
        self.unsupported_platform_response(request)
    }

    /// Handle DELETE /diagnostics/memory/dumps - Clear all dumps (unsupported)
    pub(crate) async fn handle_clear_all_dumps(
        &self,
        request: Request,
    ) -> DiagnosticsResult<Response> {
        self.unsupported_platform_response(request)
    }
}
