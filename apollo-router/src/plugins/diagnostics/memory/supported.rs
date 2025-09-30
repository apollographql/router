//! Memory profiling implementation for supported platforms (Unix + global-allocator)

use std::ffi::CString;
use std::fs;
use std::mem;
use std::path::Path;
use std::ptr;

use axum::body::Body;
use http::Response;
use http::StatusCode;
use serde_json::json;

use super::symbol_resolver::SymbolResolver;
use crate::plugins::diagnostics::DiagnosticsError;
use crate::plugins::diagnostics::DiagnosticsResult;
use crate::plugins::diagnostics::response_builder::CacheControl;
use crate::plugins::diagnostics::response_builder::ResponseBuilder;
use crate::plugins::diagnostics::security::SecurityValidator;

/// Memory profiling service that handles memory operations
#[derive(Clone)]
pub(crate) struct MemoryService {
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

    /// Helper to control jemalloc profiling (start/stop)
    async fn control_profiling(&self, enable: bool) -> DiagnosticsResult<Response<Body>> {
        let operation = if enable { "start" } else { "stop" };
        let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
            unsafe { tikv_jemalloc_ctl::raw::write::<bool>(b"prof.active\0", enable) }
                .map_err(|e| format!("Failed to {} profiling: {}", operation, e))?;

            let active = unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0") }
                .map_err(|e| format!("Failed to verify profiling state: {}", e))?;

            if active != enable {
                return Err(format!(
                    "Failed to {} profiling - state mismatch",
                    operation
                ));
            }

            tracing::info!(
                "Memory profiling successfully {}",
                if enable { "activated" } else { "deactivated" }
            );
            Ok(())
        })
        .await
        .map_err(|e| DiagnosticsError::Internal(format!("Task failed: {}", e)))?;

        let (status_code, response) = match result {
            Ok(()) => (
                StatusCode::OK,
                json!({
                    "status": if enable { "started" } else { "stopped" },
                    "message": format!("Memory profiling {}", if enable { "activated" } else { "deactivated" })
                }),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "status": "error",
                    "message": e
                }),
            ),
        };

        self.json_response(status_code, response)
    }

    /// Handle GET /diagnostics/memory/status
    pub(crate) async fn handle_status(&self) -> DiagnosticsResult<Response<Body>> {
        let status = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
            // Read profiling status from jemalloc
            let profiling_active =
                unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0") }
                    .map_err(|e| format!("Jemalloc control error: {}", e))?;

            Ok(json!({
                "profiling_active": profiling_active,
                "status": if profiling_active { "active" } else { "inactive" },
                "platform": "linux",
                "heap_dumps_available": true
            }))
        })
        .await
        .map_err(|e| DiagnosticsError::Internal(format!("Task failed: {}", e)))
        .and_then(|r| r.map_err(DiagnosticsError::Memory))?;

        self.json_response(StatusCode::OK, status)
    }

    /// Handle POST /diagnostics/memory/start
    pub(crate) async fn handle_start(&self) -> DiagnosticsResult<Response<Body>> {
        self.control_profiling(true).await
    }

    /// Handle POST /diagnostics/memory/stop
    pub(crate) async fn handle_stop(&self) -> DiagnosticsResult<Response<Body>> {
        self.control_profiling(false).await
    }

    /// Handle POST /diagnostics/memory/dump
    pub(crate) async fn handle_dump(&self) -> DiagnosticsResult<Response<Body>> {
        tracing::info!("Memory dump requested");

        let base_output_directory = self.output_directory.clone();
        let dump_result = self.create_heap_dump(&base_output_directory).await;

        let (status_code, response) = match dump_result {
            Ok(dump_path) => self.process_successful_dump(dump_path).await,
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "status": "error",
                    "message": format!("Failed to generate heap dump: {}", e)
                }),
            ),
        };

        self.json_response(status_code, response)
    }

    /// Create a heap dump using jemalloc profiling
    async fn create_heap_dump(
        &self,
        base_output_directory: &str,
    ) -> Result<String, DiagnosticsError> {
        // Create the dump path (async directory creation)
        let dump_path = Self::create_dump_path(base_output_directory)
            .await
            .map_err(DiagnosticsError::Memory)?;

        let dump_path_clone = dump_path.clone();
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            Self::call_jemalloc_dump(&dump_path_clone)?;
            tracing::info!("Memory heap dump generated at: {}", dump_path_clone);
            Ok(dump_path_clone)
        })
        .await
        .map_err(|e| DiagnosticsError::Internal(format!("Task failed: {}", e)))
        .and_then(|r| r.map_err(DiagnosticsError::Memory))
    }

    /// Create the dump file path with timestamp and ensure directory exists
    async fn create_dump_path(base_output_directory: &str) -> Result<String, String> {
        // Generate timestamp for the dump file
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("Failed to get timestamp: {}", e))?
            .as_secs();

        // Create memory subdirectory structure to mirror archive
        let base_path = Path::new(base_output_directory);
        let memory_path = base_path.join("memory");
        tokio::fs::create_dir_all(&memory_path).await.map_err(|e| {
            format!(
                "Failed to create memory dump directory {}: {}",
                memory_path.display(),
                e
            )
        })?;

        let dump_path = memory_path
            .join(format!("router_heap_dump_{}.prof", timestamp))
            .to_string_lossy()
            .to_string();

        Ok(dump_path)
    }

    /// Call jemalloc's prof.dump to create the heap dump
    fn call_jemalloc_dump(dump_path: &str) -> Result<(), String> {
        // Create CString for the dump path
        let value =
            CString::new(dump_path).map_err(|e| format!("Failed to create CString: {}", e))?;

        // Call jemalloc to dump heap profile
        let mut value_ptr = value.as_ptr();
        let result = unsafe {
            tikv_jemalloc_sys::mallctl(
                b"prof.dump\0" as *const _ as *const libc::c_char,
                ptr::null_mut::<libc::c_void>(),
                ptr::null_mut(),
                &mut value_ptr as *mut _ as *mut libc::c_void,
                mem::size_of::<*const libc::c_char>(),
            )
        };

        if result != 0 {
            return Err(format!("prof.dump failed with code: {}", result));
        }

        Ok(())
    }

    /// Process a successful dump by enhancing it and creating the response
    async fn process_successful_dump(&self, dump_path: String) -> (StatusCode, serde_json::Value) {
        // Enhance the dump in-place with embedded symbols
        let enhancement_result = self.enhance_dump(&dump_path).await;

        match enhancement_result {
            Ok(()) => (
                StatusCode::OK,
                json!({
                    "status": "dumped",
                    "message": "Enhanced heap profile generated with embedded symbols",
                    "dump_path": dump_path
                }),
            ),
            Err(e) => {
                tracing::warn!("Failed to create enhanced profile: {}", e);
                (
                    StatusCode::OK,
                    json!({
                        "status": "dumped",
                        "message": "Heap dump generated (enhanced profile failed)",
                        "dump_path": dump_path,
                        "enhancement_error": e.to_string()
                    }),
                )
            }
        }
    }

    /// Enhance a heap profile by appending symbols in-place
    async fn enhance_dump(&self, dump_path: &str) -> DiagnosticsResult<()> {
        // Get the current binary path
        let binary_path = SymbolResolver::current_binary_path()?;

        // Create enhanced processor (loads heap profile once)
        let processor = SymbolResolver::new(binary_path, dump_path).await?;

        // Enhance profile in-place
        processor.enhance_heap_profile(dump_path).await?;

        Ok(())
    }

    /// Adds memory diagnostic data to an existing tar archive with async streaming I/O
    pub(crate) async fn add_to_archive<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        output_directory: &str,
    ) -> DiagnosticsResult<()> {
        // The memory files are stored in output_directory/memory/
        let memory_directory = Path::new(output_directory).join("memory");

        if memory_directory.exists() {
            tracing::info!(
                "Adding memory diagnostic files from: {}",
                memory_directory.display()
            );

            // Stream memory files asynchronously without loading into memory
            Self::add_directory_contents_async(tar, &memory_directory, "memory")
                .await
                .map_err(|e| format!("Failed to add memory diagnostic files: {}", e))?;
        } else {
            tracing::warn!(
                "Memory diagnostic directory does not exist: {}",
                memory_directory.display()
            );

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
        }

        Ok(())
    }

    /// Recursively add directory contents to archive with async I/O
    async fn add_directory_contents_async<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        dir_path: &Path,
        archive_prefix: &str,
    ) -> Result<(), std::io::Error> {
        use tokio::fs;
        use tokio_stream::StreamExt;
        use tokio_stream::wrappers::ReadDirStream;

        let mut entries = ReadDirStream::new(fs::read_dir(dir_path).await?);

        while let Some(entry) = entries.next().await {
            let entry = entry?;
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid file name")
                })?;

            let archive_path = if archive_prefix.is_empty() {
                file_name.to_string()
            } else {
                format!("{}/{}", archive_prefix, file_name)
            };

            let metadata = entry.metadata().await?;

            if metadata.is_file() {
                // Stream file contents asynchronously
                let file = fs::File::open(&path).await?;
                let mut header = tokio_tar::Header::new_gnu();
                header.set_path(&archive_path)?;
                header.set_size(metadata.len());
                header.set_mode(0o644);
                header.set_cksum();

                // Convert tokio file to futures-compatible and stream to archive
                tar.append(&header, file).await?;

                tracing::debug!("Added memory file to archive: {}", archive_path);
            } else if metadata.is_dir() {
                // Recursively handle subdirectories
                Box::pin(Self::add_directory_contents_async(
                    tar,
                    &path,
                    &archive_path,
                ))
                .await?;
            }
        }

        Ok(())
    }

    /// Handle GET /diagnostics/memory/dumps - List all available heap dump files
    pub(crate) async fn handle_list_dumps(&self) -> DiagnosticsResult<Response<Body>> {
        let output_directory = self.output_directory.clone();

        let dumps =
            tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Value>, String> {
                let memory_path = Path::new(&output_directory).join("memory");
                let mut dumps = Vec::new();

                if memory_path.exists() {
                    let entries = fs::read_dir(&memory_path)
                        .map_err(|e| format!("Failed to read memory directory: {}", e))?;

                    for entry in entries {
                        let entry =
                            entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
                        let path = entry.path();

                        if path.is_file()
                            && path.extension().is_some_and(|ext| ext == "prof")
                            && let Some(file_name) = path.file_name().and_then(|n| n.to_str())
                        {
                            let metadata = fs::metadata(&path)
                                .map_err(|e| format!("Failed to read file metadata: {}", e))?;

                            // Use the file's created timestamp (Unix timestamp in seconds)
                            let timestamp = metadata
                                .created()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs());

                            dumps.push(serde_json::json!({
                                "name": file_name,
                                "size": metadata.len(),
                                "timestamp": timestamp,
                                "created": timestamp
                            }));
                        }
                    }

                    // Sort by creation time (newest first)
                    dumps.sort_by_key(|dump| {
                        std::cmp::Reverse(dump.get("created").and_then(|v| v.as_u64()).unwrap_or(0))
                    });
                }

                Ok(dumps)
            })
            .await
            .map_err(|e| DiagnosticsError::Internal(format!("Task failed: {}", e)))
            .and_then(|r| r.map_err(DiagnosticsError::Internal))?;

        self.json_response(StatusCode::OK, serde_json::json!(dumps))
    }

    /// Handle GET /diagnostics/memory/dumps/{filename} - Download a specific heap dump file
    pub(crate) async fn handle_download_dump(
        &self,
        filename: &str,
    ) -> DiagnosticsResult<Response<Body>> {
        // SECURITY: Critical security validation for file downloads
        if let Err(security_error) = SecurityValidator::validate_memory_dump_filename(filename) {
            return Err(security_error.into());
        }

        let memory_path = Path::new(&self.output_directory)
            .join("memory")
            .join(filename);

        // SECURITY: File existence validation
        if let Err(security_error) =
            SecurityValidator::validate_file_exists_and_is_file(&memory_path, filename)
        {
            return Err(security_error.into());
        }

        // Read and serve the file
        match tokio::fs::read(&memory_path).await {
            Ok(file_contents) => ResponseBuilder::binary_response(
                StatusCode::OK,
                "application/octet-stream",
                file_contents,
                Some(filename),
                CacheControl::NoCache,
            ),
            Err(e) => self.json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "error": "Failed to read file",
                    "message": e.to_string()
                }),
            ),
        }
    }

    /// Handle DELETE /diagnostics/memory/dumps/{filename} - Delete a specific heap dump file
    pub(crate) async fn handle_delete_dump(
        &self,
        filename: &str,
    ) -> DiagnosticsResult<Response<Body>> {
        let memory_path = Path::new(&self.output_directory)
            .join("memory")
            .join(filename);

        // SECURITY: Critical security validation for file deletion
        if let Err(security_error) =
            SecurityValidator::validate_file_deletion(&memory_path, filename, &[".prof"])
        {
            return Err(security_error.into());
        }

        // Delete the file
        match tokio::fs::remove_file(&memory_path).await {
            Ok(()) => {
                tracing::info!("Deleted heap dump: {}", filename);
                self.json_response(
                    StatusCode::OK,
                    serde_json::json!({
                        "status": "deleted",
                        "message": format!("Heap dump '{}' deleted successfully", filename)
                    }),
                )
            }
            Err(e) => {
                tracing::error!("Failed to delete heap dump {}: {}", filename, e);
                self.json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({
                        "error": "Failed to delete file",
                        "message": e.to_string()
                    }),
                )
            }
        }
    }

    /// Handle DELETE /diagnostics/memory/dumps - clear all heap dump files
    pub(crate) async fn handle_clear_all_dumps(&self) -> DiagnosticsResult<Response<Body>> {
        let memory_path = Path::new(&self.output_directory).join("memory");

        // Create memory directory if it doesn't exist
        if let Err(e) = tokio::fs::create_dir_all(&memory_path).await {
            tracing::error!("Failed to create memory directory: {}", e);
            return self.json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "error": "Failed to create memory directory",
                    "message": e.to_string()
                }),
            );
        }

        // Find all .prof files in the memory directory
        let prof_files = match tokio::fs::read_dir(&memory_path).await {
            Ok(mut entries) => {
                let mut files = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.is_file() && path.extension().is_some_and(|e| e == "prof") {
                        files.push(path);
                    }
                }
                files
            }
            Err(e) => {
                tracing::error!("Failed to read memory directory: {}", e);
                return self.json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({
                        "error": "Failed to read memory directory",
                        "message": e.to_string()
                    }),
                );
            }
        };

        let mut deleted_count = 0;
        let mut errors = Vec::new();

        // Delete each .prof file
        for prof_file in prof_files {
            match tokio::fs::remove_file(&prof_file).await {
                Ok(()) => {
                    deleted_count += 1;
                    if let Some(filename) = prof_file.file_name() {
                        tracing::info!("Deleted heap dump: {:?}", filename);
                    }
                }
                Err(e) => {
                    let filename = prof_file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    let error_msg = format!("Failed to delete {}: {}", filename, e);
                    tracing::error!("{}", error_msg);
                    errors.push(error_msg);
                }
            }
        }

        if errors.is_empty() {
            self.json_response(
                StatusCode::OK,
                serde_json::json!({
                    "status": "cleared",
                    "message": format!("Successfully deleted {} heap dump files", deleted_count),
                    "deleted_count": deleted_count
                }),
            )
        } else {
            self.json_response(
                StatusCode::PARTIAL_CONTENT,
                serde_json::json!({
                    "status": "partially_cleared",
                    "message": format!("Deleted {} files, {} errors occurred", deleted_count, errors.len()),
                    "deleted_count": deleted_count,
                    "errors": errors
                }),
            )
        }
    }
}
