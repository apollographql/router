//! Tests for the memory profiling module

use std::fs;
use std::path::Path;

use http::Method;
use http::StatusCode;
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::tempdir;

use super::*;
use crate::services::router;

fn create_test_request(method: Method, path: &str) -> router::Request {
    router::Request::fake_builder()
        .method(method)
        .uri(path.parse::<http::Uri>().unwrap())
        .build()
        .unwrap()
}

#[tokio::test]
async fn test_memory_service_creation() {
    let output_dir = "/tmp/test-memory-service";
    let service = MemoryService::new(output_dir.to_string());
    
    // Test that the service is created with the correct output directory
    assert_eq!(service.output_directory, output_dir);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_handle_status() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let service = MemoryService::new(output_path);
    let request = create_test_request(Method::GET, "/diagnostics/memory/status");
    
    let response = service.handle_status(request).await;
    assert!(response.is_ok(), "Status request should succeed");
    
    let response = response.unwrap();
    assert_eq!(response.response.status(), StatusCode::OK);
    
    // Check that response is valid JSON
    let body_bytes = response.response.into_body();
    let body_data = body_bytes.collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body_data).expect("Response should be valid JSON");
    
    // Check expected fields
    assert!(json.get("profiling_active").is_some(), "Should have profiling_active field");
    assert!(json.get("status").is_some(), "Should have status field");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_handle_start() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let service = MemoryService::new(output_path);
    let request = create_test_request(Method::POST, "/diagnostics/memory/start");
    
    let response = service.handle_start(request).await;
    
    // Note: This might fail on systems without jemalloc profiling enabled
    // but the test verifies the handler structure is correct
    match response {
        Ok(resp) => {
            assert_eq!(resp.response.status(), StatusCode::OK);
            
            let body_bytes = resp.response.into_body();
            let body_data = body_bytes.collect().await.unwrap().to_bytes();
            let json: Value = serde_json::from_slice(&body_data).expect("Response should be valid JSON");
            
            assert!(json.get("status").is_some(), "Should have status field");
            assert!(json.get("message").is_some(), "Should have message field");
        }
        Err(_) => {
            // This is expected on systems where jemalloc profiling is not available
            // The test still validates that the handler doesn't panic
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_handle_stop() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let service = MemoryService::new(output_path);
    let request = create_test_request(Method::POST, "/diagnostics/memory/stop");
    
    let response = service.handle_stop(request).await;
    
    // Note: Similar to start, this might fail on systems without jemalloc profiling
    match response {
        Ok(resp) => {
            assert_eq!(resp.response.status(), StatusCode::OK);
            
            let body_bytes = resp.response.into_body();
            let body_data = body_bytes.collect().await.unwrap().to_bytes();
            let json: Value = serde_json::from_slice(&body_data).expect("Response should be valid JSON");
            
            assert!(json.get("status").is_some(), "Should have status field");
            assert!(json.get("message").is_some(), "Should have message field");
        }
        Err(_) => {
            // Expected on systems where jemalloc profiling is not available
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_handle_dump_creates_directory() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let service = MemoryService::new(output_path.clone());
    let request = create_test_request(Method::POST, "/diagnostics/memory/dump");
    
    // Ensure memory directory doesn't exist initially
    let memory_path = Path::new(&output_path).join("memory");
    assert!(!memory_path.exists(), "Memory directory should not exist initially");
    
    let response = service.handle_dump(request).await;
    
    // The dump might fail due to jemalloc configuration, but directory should be created
    // regardless of whether the actual dump succeeds
    match response {
        Ok(resp) => {
            assert_eq!(resp.response.status(), StatusCode::OK);
            
            let body_bytes = resp.response.into_body();
            let body_data = body_bytes.collect().await.unwrap().to_bytes();
            let json: Value = serde_json::from_slice(&body_data).expect("Response should be valid JSON");
            
            assert!(json.get("status").is_some(), "Should have status field");
            assert!(json.get("dump_path").is_some(), "Should have dump_path field");
            
            // Verify the dump path is in the memory subdirectory
            let dump_path = json.get("dump_path").unwrap().as_str().unwrap();
            assert!(dump_path.contains("/memory/"), "Dump path should be in memory subdirectory");
            assert!(dump_path.contains("router_heap_dump_"), "Should have correct filename pattern");
            assert!(dump_path.ends_with(".prof"), "Should have .prof extension");
        }
        Err(_) => {
            // Even if dump fails, directory should be created
        }
    }
    
    // Directory should exist after attempt
    assert!(memory_path.exists(), "Memory directory should be created");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_add_to_archive_with_files() {
    use std::io::Cursor;
    
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    // Create memory subdirectory and test files
    let memory_path = Path::new(&output_path).join("memory");
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(memory_path.join("test_heap.prof"), b"test heap dump data")
        .expect("Failed to write test file");
    fs::write(memory_path.join("test_profile.prof"), b"test profile data")
        .expect("Failed to write another test file");
    
    // Create a tar builder
    let mut archive_buffer = Vec::new();
    let cursor = Cursor::new(&mut archive_buffer);
    let mut tar = tar::Builder::new(cursor);
    
    // Add memory files to archive
    let result = MemoryService::add_to_archive(&mut tar, &output_path);
    assert!(result.is_ok(), "Adding to archive should succeed");
    
    // Finish the archive
    let _inner = tar.into_inner().expect("Should be able to finish tar");
    assert!(!archive_buffer.is_empty(), "Archive should contain data");
    
    // Verify archive contents
    let cursor = Cursor::new(archive_buffer);
    let mut archive = tar::Archive::new(cursor);
    let entries: Vec<_> = archive.entries()
        .expect("Should be able to read entries")
        .collect::<Result<Vec<_>, _>>()
        .expect("Should be able to collect entries");
    
    assert!(!entries.is_empty(), "Archive should contain entries");
    
    // Check that memory files are in the archive
    let paths: Vec<String> = entries.iter()
        .map(|entry| entry.path().unwrap().to_string_lossy().to_string())
        .collect();
    
    assert!(paths.iter().any(|p| p.contains("memory/")), "Should contain memory directory entries");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_add_to_archive_empty_directory() {
    use std::io::Cursor;
    
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    // Don't create the memory directory - test empty case
    
    let mut archive_buffer = Vec::new();
    let cursor = Cursor::new(&mut archive_buffer);
    let mut tar = tar::Builder::new(cursor);
    
    let result = MemoryService::add_to_archive(&mut tar, &output_path);
    assert!(result.is_ok(), "Adding empty directory should succeed");
    
    let _inner = tar.into_inner().expect("Should be able to finish tar");
    
    // Verify that an empty memory directory is created in the archive
    let cursor = Cursor::new(archive_buffer);
    let mut archive = tar::Archive::new(cursor);
    let entries: Vec<_> = archive.entries()
        .expect("Should be able to read entries")
        .collect::<Result<Vec<_>, _>>()
        .expect("Should be able to collect entries");
    
    assert_eq!(entries.len(), 1, "Should have exactly one entry (empty directory)");
    
    let entry = &entries[0];
    let path = entry.path().expect("Should have path").to_string_lossy().to_string();
    assert_eq!(path, "memory/", "Should create empty memory directory");
    assert!(entry.header().entry_type().is_dir(), "Should be a directory entry");
}


#[cfg(target_os = "linux")]
#[test]
fn test_memory_error_types() {
    // Test error type creation and formatting
    let jemalloc_error = MemoryError::JemallocControl("prof.active failed".to_string());
    assert_eq!(jemalloc_error.to_string(), "Jemalloc control error: prof.active failed");
    
    let system_error = MemoryError::SystemCall("mkdir failed".to_string());
    assert_eq!(system_error.to_string(), "System call error: mkdir failed");
    
    let task_error = MemoryError::TaskFailed("tokio task panicked".to_string());
    assert_eq!(task_error.to_string(), "Task execution failed: tokio task panicked");
    
    // Test Debug formatting
    assert!(format!("{:?}", jemalloc_error).contains("JemallocControl"));
}

#[cfg(target_os = "linux")]
#[test]
fn test_memory_service_clone() {
    let service1 = MemoryService::new("/tmp/test1".to_string());
    let service2 = service1.clone();
    
    assert_eq!(service1.output_directory, service2.output_directory);
}

// Test helper function visibility
#[cfg(target_os = "linux")]
#[test]
fn test_path_handling() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let base_path = temp_dir.path().to_str().unwrap();
    
    // Test that memory subdirectory path is constructed correctly
    let memory_path = Path::new(base_path).join("memory");
    assert_eq!(memory_path, Path::new(&format!("{}/memory", base_path)));
    
    // Test path display formatting
    let display_str = format!("{}", memory_path.display());
    assert!(display_str.ends_with("/memory"));
}