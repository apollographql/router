//! Heap profiling for the diagnostics plugin
//!
//! Start/stop sampling and take heap dumps on demand, built on the router's shared heap profiler
//! ([`crate::heap_profiling`]). Dumps are symbolized in-process and written as gzipped
//! pprof (`.pb.gz`), so they open in the dashboard and in standard tools (`go tool pprof`,
//! Pyroscope, speedscope) without the router binary.
//!
//! Heap profiling needs a Unix platform and the router's jemalloc allocator. Elsewhere, start,
//! stop and dump respond with `501 Not Implemented`, and the dump list is empty.

use std::path::Path;
use std::path::PathBuf;

use axum::body::Body;
use http::Response;
use http::StatusCode;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;

use super::DiagnosticsError;
use super::DiagnosticsResult;
use super::response_builder::CacheControl;
use super::response_builder::ResponseBuilder;
use super::security::SecurityValidator;
use crate::heap_profiling;

/// Heap dumps are gzipped pprof.
pub(super) const DUMP_EXTENSION: &str = ".pb.gz";

/// Sampling started from the dashboard or API, held until stopped. It belongs to the process
/// rather than to a plugin instance, so a configuration reload doesn't end it.
static SESSION: Mutex<Option<heap_profiling::Sampling>> = Mutex::new(None);

fn is_dump(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(DUMP_EXTENSION))
}

/// Names of the heap dumps in a memory directory
async fn dump_names(memory_directory: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    if memory_directory.exists() {
        let mut entries = tokio::fs::read_dir(memory_directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            if is_dump(&entry.path()) {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    Ok(names)
}

/// End a memory profiling session, if one is running.
pub(super) fn end_session() {
    if SESSION.lock().take().is_some() {
        tracing::info!("memory profiling stopped");
    }
}

/// Represents a memory dump file with its metadata and content
#[derive(Debug, Clone, Serialize)]
pub(super) struct MemoryDump {
    pub name: String,
    /// Base64 of the gzipped pprof
    pub data: String,
    pub size: u64,
    /// Unix timestamp (seconds since epoch)
    pub timestamp: Option<u64>,
}

/// Load all memory dump files from a directory
pub(super) async fn load_memory_dumps(
    memory_directory: &Path,
) -> DiagnosticsResult<Vec<MemoryDump>> {
    use base64::Engine;

    let mut dumps = Vec::new();
    for name in dump_names(memory_directory).await? {
        let path = memory_directory.join(&name);
        match tokio::fs::read(&path).await {
            Ok(content) => dumps.push(MemoryDump {
                size: content.len() as u64,
                data: base64::engine::general_purpose::STANDARD.encode(&content),
                timestamp: created(&path),
                name,
            }),
            // Continue with the other dumps
            Err(error) => tracing::warn!(dump = name, %error, "failed to read heap dump"),
        }
    }

    // Sort dumps by name for consistent ordering
    dumps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(dumps)
}

/// The file's creation time, as a Unix timestamp in seconds
fn created(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Sampling only records allocations made after it starts, so a dump taken right after starting a
/// session is empty. Say so, rather than leave an empty flame graph to explain itself.
fn dump_message(samples: usize) -> &'static str {
    if samples == 0 {
        "Heap profile written, but no allocations were sampled. Sampling only records \
         allocations made after profiling starts: send traffic, then dump again."
    } else {
        "Heap profile written as gzipped pprof"
    }
}

/// Memory profiling service that handles memory operations
#[derive(Clone)]
pub(crate) struct MemoryService {
    pub output_directory: PathBuf,
}

impl MemoryService {
    pub(crate) fn new(output_directory: &Path) -> Self {
        Self {
            output_directory: output_directory.to_owned(),
        }
    }

    fn memory_directory(&self) -> PathBuf {
        self.output_directory.join("memory")
    }

    fn json_response(
        &self,
        status: StatusCode,
        data: serde_json::Value,
    ) -> DiagnosticsResult<Response<Body>> {
        ResponseBuilder::json_response(status, &data, CacheControl::NoCache)
    }

    fn error_response(
        &self,
        status: StatusCode,
        message: String,
    ) -> DiagnosticsResult<Response<Body>> {
        self.json_response(status, json!({"status": "error", "message": message}))
    }

    /// `501 Not Implemented` where heap profiling isn't available.
    fn unavailable(&self) -> Option<DiagnosticsResult<Response<Body>>> {
        let err = heap_profiling::available().err()?;
        Some(self.json_response(
            StatusCode::NOT_IMPLEMENTED,
            json!({
                "status": "not_supported",
                "message": format!("Memory profiling is not available: {err}"),
                "platform": std::env::consts::OS,
            }),
        ))
    }

    /// Handle GET /diagnostics/memory/status
    pub(crate) async fn handle_status(&self) -> DiagnosticsResult<Response<Body>> {
        let status = match heap_profiling::available() {
            Ok(()) => {
                let active = SESSION.lock().is_some();
                json!({
                    "profiling_active": active,
                    "status": if active { "active" } else { "inactive" },
                    // Sampling can be on without a session (another user of the heap
                    // profiler, or `_RJEM_MALLOC_CONF`), in which case dumps include
                    // allocations made since it started.
                    "sampling_active": heap_profiling::is_sampling().unwrap_or(false),
                    "platform": std::env::consts::OS,
                    "heap_dumps_available": true,
                })
            }
            Err(err) => json!({
                "profiling_active": false,
                "status": "not_available",
                "platform": std::env::consts::OS,
                "heap_dumps_available": false,
                "message": format!("Memory profiling is not available: {err}"),
            }),
        };
        self.json_response(StatusCode::OK, status)
    }

    /// Handle POST /diagnostics/memory/start
    pub(crate) async fn handle_start(&self) -> DiagnosticsResult<Response<Body>> {
        if let Some(response) = self.unavailable() {
            return response;
        }
        {
            let mut session = SESSION.lock();
            if session.is_none() {
                match heap_profiling::start_sampling() {
                    Ok(sampling) => *session = Some(sampling),
                    Err(err) => {
                        return self.error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to start memory profiling: {err}"),
                        );
                    }
                }
            }
        }
        tracing::info!("memory profiling started");
        self.json_response(
            StatusCode::OK,
            json!({
                "status": "started",
                "message": "Memory profiling activated. Heap dumps include allocations made from now on, so send traffic before dumping."
            }),
        )
    }

    /// Handle POST /diagnostics/memory/stop
    pub(crate) async fn handle_stop(&self) -> DiagnosticsResult<Response<Body>> {
        if let Some(response) = self.unavailable() {
            return response;
        }
        // Sampling stays on if another user of the heap profiler still needs it.
        *SESSION.lock() = None;
        tracing::info!("memory profiling stopped");
        self.json_response(
            StatusCode::OK,
            json!({"status": "stopped", "message": "Memory profiling deactivated"}),
        )
    }

    /// Handle POST /diagnostics/memory/dump
    pub(crate) async fn handle_dump(&self) -> DiagnosticsResult<Response<Body>> {
        if let Some(response) = self.unavailable() {
            return response;
        }
        match self.write_dump().await {
            Ok((dump_path, samples)) => {
                tracing::info!(path = %dump_path.display(), samples, "heap dump written");
                self.json_response(
                    StatusCode::OK,
                    json!({
                        "status": "dumped",
                        "message": dump_message(samples),
                        "dump_path": dump_path,
                        "samples": samples,
                    }),
                )
            }
            Err(err) => self.error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to generate heap dump: {err}"),
            ),
        }
    }

    async fn write_dump(&self) -> Result<(PathBuf, usize), DiagnosticsError> {
        let heap = heap_profiling::collect_pprof()
            .await
            .map_err(|err| DiagnosticsError::Memory(err.to_string()))?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| DiagnosticsError::Internal(err.to_string()))?
            .as_secs();
        let memory_directory = self.memory_directory();
        tokio::fs::create_dir_all(&memory_directory).await?;
        let dump_path =
            memory_directory.join(format!("router_heap_dump_{timestamp}{DUMP_EXTENSION}"));
        tokio::fs::write(&dump_path, heap.pprof).await?;
        Ok((dump_path, heap.samples))
    }

    /// Adds memory diagnostic data to an existing tar archive with async streaming I/O
    pub(crate) async fn add_to_archive<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        output_directory: &Path,
    ) -> DiagnosticsResult<()> {
        let memory_directory = output_directory.join("memory");
        if memory_directory.exists() {
            Self::add_directory_contents(tar, &memory_directory, "memory")
                .await
                .map_err(|e| format!("Failed to add memory diagnostic files: {}", e))?;
        } else {
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
    async fn add_directory_contents<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        dir_path: &Path,
        archive_prefix: &str,
    ) -> Result<(), std::io::Error> {
        let mut entries = tokio::fs::read_dir(dir_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid file name")
                })?;
            let archive_path = format!("{archive_prefix}/{file_name}");

            let metadata = entry.metadata().await?;
            if metadata.is_file() {
                let file = tokio::fs::File::open(&path).await?;
                let mut header = tokio_tar::Header::new_gnu();
                header.set_path(&archive_path)?;
                header.set_size(metadata.len());
                header.set_mode(0o644);
                header.set_cksum();
                tar.append(&header, file).await?;
            } else if metadata.is_dir() {
                Box::pin(Self::add_directory_contents(tar, &path, &archive_path)).await?;
            }
        }
        Ok(())
    }

    /// Handle GET /diagnostics/memory/dumps - List all available heap dump files
    pub(crate) async fn handle_list_dumps(&self) -> DiagnosticsResult<Response<Body>> {
        let memory_directory = self.memory_directory();
        let mut dumps = Vec::new();
        for name in dump_names(&memory_directory).await? {
            let path = memory_directory.join(&name);
            let size = tokio::fs::metadata(&path).await?.len();
            let timestamp = created(&path);
            dumps.push(json!({
                "name": name,
                "size": size,
                "timestamp": timestamp,
                "created": timestamp,
            }));
        }
        // Newest first
        dumps.sort_by_key(|dump| std::cmp::Reverse(dump["created"].as_u64().unwrap_or(0)));
        self.json_response(StatusCode::OK, json!(dumps))
    }

    /// Handle GET /diagnostics/memory/dumps/{filename} - Download a specific heap dump file
    pub(crate) async fn handle_download_dump(
        &self,
        filename: &str,
    ) -> DiagnosticsResult<Response<Body>> {
        // SECURITY: Critical security validation for file downloads
        SecurityValidator::validate_memory_dump_filename(filename)?;
        let path = self.memory_directory().join(filename);
        SecurityValidator::validate_file_exists_and_is_file(&path, filename)?;

        match tokio::fs::read(&path).await {
            Ok(contents) => ResponseBuilder::binary_response(
                StatusCode::OK,
                "application/octet-stream",
                contents,
                Some(filename),
                CacheControl::NoCache,
            ),
            Err(e) => self.error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read file: {e}"),
            ),
        }
    }

    /// Delete a single heap dump file with validation
    async fn delete_single_dump(&self, filename: &str) -> Result<(), String> {
        let path = self.memory_directory().join(filename);
        // SECURITY: Critical security validation for file deletion
        SecurityValidator::validate_file_deletion(&path, filename, &[DUMP_EXTENSION])
            .map_err(|e| e.to_string())?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| format!("Failed to delete {}: {}", filename, e))?;
        tracing::info!(dump = filename, "deleted heap dump");
        Ok(())
    }

    /// Handle DELETE /diagnostics/memory/dumps/{filename} - Delete a specific heap dump file
    pub(crate) async fn handle_delete_dump(
        &self,
        filename: &str,
    ) -> DiagnosticsResult<Response<Body>> {
        match self.delete_single_dump(filename).await {
            Ok(()) => self.json_response(
                StatusCode::OK,
                json!({
                    "status": "deleted",
                    "message": format!("Heap dump '{}' deleted successfully", filename)
                }),
            ),
            Err(e) => self.error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
        }
    }

    /// Handle DELETE /diagnostics/memory/dumps - clear all heap dump files
    pub(crate) async fn handle_clear_all_dumps(&self) -> DiagnosticsResult<Response<Body>> {
        let mut deleted_count = 0;
        let mut errors = Vec::new();
        for name in dump_names(&self.memory_directory()).await? {
            match self.delete_single_dump(&name).await {
                Ok(()) => deleted_count += 1,
                Err(e) => errors.push(e),
            }
        }

        if errors.is_empty() {
            self.json_response(
                StatusCode::OK,
                json!({
                    "status": "cleared",
                    "message": format!("Successfully deleted {} heap dump files", deleted_count),
                    "deleted_count": deleted_count
                }),
            )
        } else {
            self.json_response(
                StatusCode::PARTIAL_CONTENT,
                json!({
                    "status": "partially_cleared",
                    "message": format!("Deleted {} files, {} errors occurred", deleted_count, errors.len()),
                    "deleted_count": deleted_count,
                    "errors": errors
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests;
