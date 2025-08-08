//! Memory profiling functionality for diagnostics plugin
//!
//! This module handles jemalloc memory profiling operations including:
//! - Starting/stopping profiling
//! - Generating heap dumps
//! - Querying profiling status
//!
//! **Platform Support**: This module is only available on Linux platforms.

use std::ffi::CString;
use std::fs;
use std::mem;
use std::path::Path;
use std::ptr;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use http::StatusCode;
use serde_json::json;
use thiserror::Error;
use tower::BoxError;

use crate::services::router::Request;
use crate::services::router::Response;
use crate::services::router::body;

#[cfg(test)]
mod tests;

/// Errors that can occur during memory profiling operations
#[derive(Debug, Error)]
pub(super) enum MemoryError {
    /// Jemalloc control operation failed
    #[error("Jemalloc control error: {0}")]
    JemallocControl(String),
    /// System call failed
    #[error("System call error: {0}")]
    SystemCall(String),
    /// Profiling not available or not compiled with profiling support
    #[error("Memory profiling not available")]
    #[allow(dead_code)]
    ProfilingNotAvailable,
    /// Task execution failed
    #[error("Task execution failed: {0}")]
    TaskFailed(String),
}

/// Memory profiling service that handles jemalloc operations
#[derive(Clone)]
pub(super) struct MemoryService {
    output_directory: String,
}

impl MemoryService {
    pub(super) fn new(output_directory: String) -> Self {
        Self {
            output_directory,
        }
    }

    /// Handle GET /diagnostics/memory/status
    pub(super) async fn handle_status(&self, request: Request) -> Result<Response, BoxError> {
        // Use tokio spawn_blocking for jemalloc operations
        let status = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, MemoryError> {
            // Read profiling status from jemalloc
            let profiling_active = unsafe {
                tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0")
            }.map_err(|e| MemoryError::JemallocControl(e.to_string()))?;
            
            Ok(json!({
                "profiling_active": profiling_active,
                "status": if profiling_active { "active" } else { "inactive" }
            }))
        })
        .await
        .map_err(|e| MemoryError::TaskFailed(format!("Task failed: {}", e)))
        .and_then(|r| r)
        .map_err(|e| BoxError::from(e.to_string()))?;

        Ok(Response::http_response_builder()
            .response(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(body::from_bytes(serde_json::to_vec(&status)?))?,
            )
            .context(request.context)
            .build()?)
    }

    /// Handle POST /diagnostics/memory/start
    pub(super) async fn handle_start(&self, request: Request) -> Result<Response, BoxError> {
        tracing::info!("Memory profiling start requested");
        
        // Use tokio spawn_blocking for jemalloc operations
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            // Enable jemalloc profiling
            unsafe {
                tikv_jemalloc_ctl::raw::write::<bool>(b"prof.active\0", true)
            }.map_err(|e| MemoryError::JemallocControl(e.to_string()))?;
            
            // Verify it was enabled
            let active = unsafe {
                tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0")
            }.map_err(|e| MemoryError::JemallocControl(e.to_string()))?;
            
            if !active {
                return Err(MemoryError::JemallocControl("Failed to activate profiling".to_string()));
            }
            
            tracing::info!("Memory profiling successfully activated");
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::TaskFailed(format!("Task failed: {}", e)))
        .and_then(|r| r)
        .map_err(|e| BoxError::from(e.to_string()))?;

        let response = json!({
            "status": "started",
            "message": "Memory profiling activated"
        });

        Ok(Response::http_response_builder()
            .response(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(body::from_bytes(serde_json::to_vec(&response)?))?,
            )
            .context(request.context)
            .build()?)
    }

    /// Handle POST /diagnostics/memory/stop
    pub(super) async fn handle_stop(&self, request: Request) -> Result<Response, BoxError> {
        tracing::info!("Memory profiling stop requested");
        
        // Use tokio spawn_blocking for jemalloc operations
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            // Disable jemalloc profiling
            unsafe {
                tikv_jemalloc_ctl::raw::write::<bool>(b"prof.active\0", false)
            }.map_err(|e| MemoryError::JemallocControl(e.to_string()))?;
            
            // Verify it was disabled
            let active = unsafe {
                tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0")
            }.map_err(|e| MemoryError::JemallocControl(e.to_string()))?;
            
            if active {
                return Err(MemoryError::JemallocControl("Failed to deactivate profiling".to_string()));
            }
            
            tracing::info!("Memory profiling successfully deactivated");
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::TaskFailed(format!("Task failed: {}", e)))
        .and_then(|r| r)
        .map_err(|e| BoxError::from(e.to_string()))?;

        let response = json!({
            "status": "stopped",
            "message": "Memory profiling deactivated"
        });

        Ok(Response::http_response_builder()
            .response(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(body::from_bytes(serde_json::to_vec(&response)?))?,
            )
            .context(request.context)
            .build()?)
    }

    /// Handle POST /diagnostics/memory/dump
    pub(super) async fn handle_dump(&self, request: Request) -> Result<Response, BoxError> {
        tracing::info!("Memory dump requested");
        
        let base_output_directory = self.output_directory.clone();
        
        // Use tokio spawn_blocking for jemalloc operations
        let dump_path = tokio::task::spawn_blocking(move || -> Result<String, MemoryError> {
            // Generate timestamp for the dump file
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| MemoryError::SystemCall(e.to_string()))?
                .as_secs();
            
            // Create memory subdirectory structure to mirror archive
            let base_path = Path::new(&base_output_directory);
            let memory_path = base_path.join("memory");
            fs::create_dir_all(&memory_path)
                .map_err(|e| MemoryError::SystemCall(format!("Failed to create memory dump directory {}: {}", memory_path.display(), e)))?;
            
            let dump_path = memory_path.join(format!("router_heap_dump_{}.prof", timestamp))
                .to_string_lossy()
                .to_string();
            
            // Create CString for the dump path as shown in the example
            let value = CString::new(dump_path.clone())
                .map_err(|e| MemoryError::SystemCall(format!("Failed to create CString: {}", e)))?;

            // Call jemalloc to dump heap profile using the proper approach from the example
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
                return Err(MemoryError::JemallocControl(format!("prof.dump failed with code: {}", result)));
            }
            
            tracing::info!("Memory heap dump generated at: {}", dump_path);
            Ok(dump_path)
        })
        .await
        .map_err(|e| MemoryError::TaskFailed(format!("Task failed: {}", e)))
        .and_then(|r| r)
        .map_err(|e| BoxError::from(e.to_string()))?;

        let response = json!({
            "status": "dumped",
            "message": "Heap dump generated",
            "dump_path": dump_path
        });

        Ok(Response::http_response_builder()
            .response(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(body::from_bytes(serde_json::to_vec(&response)?))?,
            )
            .context(request.context)
            .build()?)
    }


    /// Adds memory diagnostic data to an existing tar archive
    /// This method is called by the main diagnostics export handler
    pub(super) fn add_to_archive<W: std::io::Write>(tar: &mut tar::Builder<W>, output_directory: &str) -> Result<(), BoxError> {
        // The memory files are stored in output_directory/memory/
        let memory_directory = Path::new(output_directory).join("memory");
        
        if memory_directory.exists() {
            tracing::info!("Adding memory diagnostic files from: {}", memory_directory.display());
            tar.append_dir_all("memory", &memory_directory)
                .map_err(|e| format!("Failed to add memory diagnostic files: {}", e))?;
        } else {
            tracing::warn!("Memory diagnostic directory does not exist: {}", memory_directory.display());
            
            // Create empty memory directory in archive
            let mut header = tar::Header::new_gnu();
            header.set_path("memory/")
                .map_err(|e| format!("Failed to set memory directory path: {}", e))?;
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            
            tar.append(&header, std::io::empty())
                .map_err(|e| format!("Failed to add memory directory: {}", e))?;
        }

        Ok(())
    }

}
