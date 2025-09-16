//! Memory profiling functionality for diagnostics plugin
//!
//! This module provides a unified interface for memory profiling operations using
//! platform-specific implementations:
//!
//! - **Supported platforms** (Unix + global-allocator): Full jemalloc integration
//!   - Starting/stopping profiling
//!   - Generating heap dumps
//!   - Querying profiling status
//!   - Archive integration
//!
//! - **Unsupported platforms**: Graceful degradation with stub implementations
//!   - Returns appropriate "not supported" responses
//!   - Maintains API compatibility
//!
//! The implementation automatically selects the appropriate backend based on
//! compile-time feature detection.

use serde::Serialize;

use super::DiagnosticsResult;

/// Represents a memory dump file with its metadata and content
#[derive(Debug, Clone, Serialize)]
pub(super) struct MemoryDump {
    pub name: String,
    pub data: String,
    pub size: u64,
    pub timestamp: Option<String>,
}

impl MemoryDump {
    /// Extract timestamp from heap dump filename (router_heap_dump_TIMESTAMP.prof)
    pub(super) fn extract_timestamp_from_filename(filename: &str) -> Option<String> {
        // Look for pattern: router_heap_dump_TIMESTAMP.prof
        if let Some(start) = filename.find("router_heap_dump_") {
            let timestamp_str = filename
                .get(start + "router_heap_dump_".len()..)?
                .split('.')
                .next()?;

            // Try to parse as Unix timestamp and convert to human readable
            let timestamp = timestamp_str.parse::<i64>().ok()?;
            chrono::DateTime::from_timestamp(timestamp, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        } else {
            None
        }
    }
}

/// Load all memory dump files from a directory
pub(super) async fn load_memory_dumps(
    memory_directory: &std::path::Path,
) -> DiagnosticsResult<Vec<MemoryDump>> {
    use tokio::fs;

    let mut dumps = Vec::new();

    if memory_directory.exists() {
        let mut entries = fs::read_dir(memory_directory).await.map_err(|e| {
            super::DiagnosticsError::Internal(format!("Failed to read memory directory: {}", e))
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            super::DiagnosticsError::Internal(format!("Failed to read directory entry: {}", e))
        })? {
            let path = entry.path();

            // Process .prof files
            if path.is_file()
                && path.extension().is_some_and(|ext| ext == "prof")
                && let Some(file_name) = path.file_name().and_then(|n| n.to_str())
            {
                match load_single_memory_dump(&path, file_name).await {
                    Ok(dump) => dumps.push(dump),
                    Err(e) => {
                        tracing::warn!("Failed to process dump {}: {}", file_name, e);
                        // Continue processing other dumps even if one fails
                    }
                }
            }
        }
    }

    // Sort dumps by name for consistent ordering
    dumps.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(dumps)
}

/// Load a single memory dump file
async fn load_single_memory_dump(
    path: &std::path::Path,
    file_name: &str,
) -> DiagnosticsResult<MemoryDump> {
    use base64::Engine;
    use tokio::fs;

    // Read the file content
    let content = fs::read(path).await.map_err(|e| {
        super::DiagnosticsError::Internal(format!("Failed to read dump file {}: {}", file_name, e))
    })?;

    // Get file metadata
    let metadata = fs::metadata(path).await.map_err(|e| {
        super::DiagnosticsError::Internal(format!(
            "Failed to read dump metadata {}: {}",
            file_name, e
        ))
    })?;

    // Encode content as base64
    let encoded_content = base64::engine::general_purpose::STANDARD.encode(&content);

    // Extract timestamp from filename if possible
    let timestamp = MemoryDump::extract_timestamp_from_filename(file_name);

    Ok(MemoryDump {
        name: file_name.to_string(),
        size: metadata.len(),
        data: encoded_content,
        timestamp,
    })
}

// Conditional module imports based on platform support
#[cfg(all(target_family = "unix", feature = "global-allocator"))]
mod supported;
#[cfg(not(all(target_family = "unix", feature = "global-allocator")))]
mod unsupported;

// Enhanced heap processing module - available on all platforms
pub(super) mod symbol_resolver;

// Conditional re-exports using platform-appropriate implementation
#[cfg(all(target_family = "unix", feature = "global-allocator"))]
pub(super) use supported::MemoryService;
#[cfg(not(all(target_family = "unix", feature = "global-allocator")))]
pub(super) use unsupported::MemoryService;

#[cfg(all(target_family = "unix", feature = "global-allocator"))]
#[cfg(test)]
mod tests;
