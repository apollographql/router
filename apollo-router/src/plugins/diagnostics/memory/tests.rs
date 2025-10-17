//! Tests for the memory profiling module

use std::fs;
use std::path::Path;

use futures::TryStreamExt;
use http::StatusCode;
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::tempdir;
use tokio::io::AsyncReadExt;

use super::*;

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_handle_status() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let service = MemoryService::new(output_path);

    let response = service.handle_status().await;
    assert!(response.is_ok(), "Status request should succeed");

    let response = response.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Check that response is valid JSON
    let body_bytes = response.into_body();
    let body_data = body_bytes.collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body_data).expect("Response should be valid JSON");

    // Check expected fields
    assert!(
        json.get("profiling_active").is_some(),
        "Should have profiling_active field"
    );
    assert!(json.get("status").is_some(), "Should have status field");
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_handle_start() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let service = MemoryService::new(output_path);

    let response = service.handle_start().await;

    // Note: This might fail on systems without jemalloc profiling enabled
    // but the test verifies the handler structure is correct
    match response {
        Ok(resp) => {
            // With global-allocator feature: expect OK or INTERNAL_SERVER_ERROR
            // Without global-allocator feature: expect NOT_IMPLEMENTED
            #[cfg(all(target_family = "unix", feature = "global-allocator"))]
            {
                assert!(
                    resp.status() == StatusCode::OK
                        || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
                    "Expected OK or INTERNAL_SERVER_ERROR, got: {}",
                    resp.status()
                );
            }
            #[cfg(not(all(target_family = "unix", feature = "global-allocator")))]
            {
                assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
            }

            let body_bytes = resp.into_body();
            let body_data = body_bytes.collect().await.unwrap().to_bytes();
            let json: Value =
                serde_json::from_slice(&body_data).expect("Response should be valid JSON");

            assert!(json.get("status").is_some(), "Should have status field");
            assert!(json.get("message").is_some(), "Should have message field");
        }
        Err(_) => {
            // This is expected on systems where jemalloc profiling is not available
            // The test still validates that the handler doesn't panic
        }
    }
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_handle_stop() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let service = MemoryService::new(output_path);

    let response = service.handle_stop().await;

    // Note: Similar to start, this might fail on systems without jemalloc profiling
    match response {
        Ok(resp) => {
            // With global-allocator feature: expect OK or INTERNAL_SERVER_ERROR
            // Without global-allocator feature: expect NOT_IMPLEMENTED
            #[cfg(all(target_family = "unix", feature = "global-allocator"))]
            {
                assert!(
                    resp.status() == StatusCode::OK
                        || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
                    "Expected OK or INTERNAL_SERVER_ERROR, got: {}",
                    resp.status()
                );
            }
            #[cfg(not(all(target_family = "unix", feature = "global-allocator")))]
            {
                assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
            }

            let body_bytes = resp.into_body();
            let body_data = body_bytes.collect().await.unwrap().to_bytes();
            let json: Value =
                serde_json::from_slice(&body_data).expect("Response should be valid JSON");

            assert!(json.get("status").is_some(), "Should have status field");
            assert!(json.get("message").is_some(), "Should have message field");
        }
        Err(_) => {
            // Expected on systems where jemalloc profiling is not available
        }
    }
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_handle_dump_creates_directory() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let service = MemoryService::new(output_path.clone());

    // Ensure memory directory doesn't exist initially
    let memory_path = Path::new(&output_path).join("memory");
    assert!(
        !memory_path.exists(),
        "Memory directory should not exist initially"
    );

    let response = service.handle_dump().await;

    // The dump might fail due to jemalloc configuration, but directory should be created
    // regardless of whether the actual dump succeeds
    match response {
        Ok(resp) => {
            // With global-allocator feature: expect OK or INTERNAL_SERVER_ERROR
            // Without global-allocator feature: expect NOT_IMPLEMENTED
            #[cfg(all(target_family = "unix", feature = "global-allocator"))]
            {
                assert!(
                    resp.status() == StatusCode::OK
                        || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
                    "Expected OK or INTERNAL_SERVER_ERROR, got: {}",
                    resp.status()
                );
            }
            #[cfg(not(all(target_family = "unix", feature = "global-allocator")))]
            {
                assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
            }

            let status = resp.status();
            let body_bytes = resp.into_body();
            let body_data = body_bytes.collect().await.unwrap().to_bytes();
            let json: Value =
                serde_json::from_slice(&body_data).expect("Response should be valid JSON");

            assert!(json.get("status").is_some(), "Should have status field");

            // Only check for dump_path if global-allocator is enabled and operation succeeded
            #[cfg(all(target_family = "unix", feature = "global-allocator"))]
            {
                if status == StatusCode::OK {
                    assert!(
                        json.get("dump_path").is_some(),
                        "Should have dump_path field"
                    );
                }
            }

            // Only verify dump path details if global-allocator feature is enabled
            #[cfg(all(target_family = "unix", feature = "global-allocator"))]
            {
                if status == StatusCode::OK {
                    let dump_path = json.get("dump_path").unwrap().as_str().unwrap();
                    assert!(
                        dump_path.contains("/memory/"),
                        "Dump path should be in memory subdirectory"
                    );
                    assert!(
                        dump_path.contains("router_heap_dump_"),
                        "Should have correct filename pattern"
                    );
                    assert!(dump_path.ends_with(".prof"), "Should have .prof extension");
                }
            }
        }
        Err(_) => {
            // Even if dump fails, directory should be created
        }
    }

    // Directory should exist after attempt only if global-allocator is enabled
    #[cfg(all(target_family = "unix", feature = "global-allocator"))]
    assert!(memory_path.exists(), "Memory directory should be created");

    #[cfg(not(all(target_family = "unix", feature = "global-allocator")))]
    {
        // Without global-allocator, the directory won't be created
        // This is expected behavior
    }
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_add_to_archive_with_files() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    // Create memory subdirectory and test files
    let memory_path = Path::new(&output_path).join("memory");
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(memory_path.join("test_heap.prof"), b"test heap dump data")
        .expect("Failed to write test file");
    fs::write(memory_path.join("test_profile.prof"), b"test profile data")
        .expect("Failed to write another test file");

    // Create a tar builder using duplex stream
    let (mut reader, writer) = tokio::io::duplex(1024 * 1024);
    let mut tar = tokio_tar::Builder::new(writer);

    // Add memory files to archive
    let result = MemoryService::add_to_archive(&mut tar, &output_path).await;
    assert!(result.is_ok(), "Adding to archive should succeed");

    // Finish the archive
    tar.finish().await.expect("Should be able to finish tar");
    drop(tar); // Close the writer side

    // Read archive data
    let mut archive_data = Vec::new();
    reader
        .read_to_end(&mut archive_data)
        .await
        .expect("Should be able to read data");
    assert!(!archive_data.is_empty(), "Archive should contain data");

    // Verify archive contents
    let mut archive = tokio_tar::Archive::new(archive_data.as_slice());
    let mut entries_stream = archive.entries().expect("Should be able to read entries");
    let mut entries = Vec::new();

    while let Some(entry) = entries_stream.try_next().await.expect("Should read entry") {
        entries.push(entry);
    }

    assert!(!entries.is_empty(), "Archive should contain entries");

    // Check that memory files are in the archive
    let paths: Vec<String> = entries
        .iter()
        .map(|entry: &tokio_tar::Entry<_>| entry.path().unwrap().to_string_lossy().to_string())
        .collect();

    assert!(
        paths.iter().any(|p| p.contains("memory/")),
        "Should contain memory directory entries"
    );
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_add_to_archive_empty_directory() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    // Don't create the memory directory - test empty case

    let (mut reader, writer) = tokio::io::duplex(1024 * 1024);
    let mut tar = tokio_tar::Builder::new(writer);

    let result = MemoryService::add_to_archive(&mut tar, &output_path).await;
    assert!(result.is_ok(), "Adding empty directory should succeed");

    tar.finish().await.expect("Should be able to finish tar");
    drop(tar); // Close the writer side

    // Read archive data
    let mut archive_data = Vec::new();
    reader
        .read_to_end(&mut archive_data)
        .await
        .expect("Should be able to read data");

    // Verify that an empty memory directory is created in the archive
    let mut archive = tokio_tar::Archive::new(archive_data.as_slice());
    let mut entries_stream = archive.entries().expect("Should be able to read entries");
    let mut entries = Vec::new();

    while let Some(entry) = entries_stream.try_next().await.expect("Should read entry") {
        entries.push(entry);
    }

    assert_eq!(
        entries.len(),
        1,
        "Should have exactly one entry (empty directory)"
    );

    let entry: &tokio_tar::Entry<_> = &entries[0];
    let path = entry
        .path()
        .expect("Should have path")
        .to_string_lossy()
        .to_string();
    assert_eq!(path, "memory/", "Should create empty memory directory");
    assert!(
        entry.header().entry_type().is_dir(),
        "Should be a directory entry"
    );
}

#[cfg(target_family = "unix")]
#[test]
fn test_memory_service_clone() {
    let service1 = MemoryService::new("/tmp/test1".to_string());
    let service2 = service1.clone();

    assert_eq!(service1.output_directory, service2.output_directory);
}

// Test helper function visibility
#[cfg(target_family = "unix")]
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
