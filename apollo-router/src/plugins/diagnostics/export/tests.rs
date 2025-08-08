//! Tests for the export module

use std::fs;
use std::path::Path;

use flate2::read::GzDecoder;
use tar::Archive;
use tempfile::tempdir;

use super::*;
use crate::plugins::diagnostics::{default_diagnostics_listen, default_output_directory};

#[tokio::test]
async fn test_export_service_creation() {
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: default_output_directory(),
    };

    let test_config_json = serde_json::json!({
        "test": "configuration"
    });
    let export_service = ExportService::new(config.clone(), Some(test_config_json));
    assert_eq!(export_service.config().output_directory, config.output_directory);
    assert_eq!(export_service.config().shared_secret, config.shared_secret);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_create_comprehensive_archive() {
    // Create a temporary directory for test files
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    // Create some test files in the memory subdirectory to mirror the new structure
    let memory_path = format!("{}/memory", output_path);
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(format!("{}/test_heap.prof", memory_path), b"test heap dump data").expect("Failed to write test file");
    fs::write(format!("{}/test_profile.prof", memory_path), b"test profile data").expect("Failed to write another test file");
    
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: output_path,
    };

    // Create the archive with test configuration
    let test_full_config = Some(serde_json::json!({
        "experimental_diagnostics": {
            "enabled": true,
            "shared_secret": "test-secret"
        }
    }));
    let result = ExportService::create_archive(&config, &test_full_config);
    assert!(result.is_ok(), "Archive creation should succeed");
    
    let archive_data = result.unwrap();
    assert!(!archive_data.is_empty(), "Archive should not be empty");
    
    // Verify the archive can be decompressed and read
    let cursor = std::io::Cursor::new(archive_data);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);
    
    let mut entries = archive.entries().expect("Should be able to read archive entries");
    let mut found_manifest = false;
    let mut found_router_yaml = false;
    let mut found_memory_dir = false;
    let mut found_binary = false;
    
    while let Some(entry) = entries.next() {
        let entry = entry.expect("Should be able to read entry");
        let path = entry.path().expect("Should have path").to_string_lossy().to_string();
        
        match path.as_str() {
            "manifest.txt" => found_manifest = true,
            "router.yaml" => found_router_yaml = true,
            "router-binary.txt" => found_binary = true, // In test mode
            path if path.starts_with("memory/") => found_memory_dir = true,
            _ => {} // Other files are okay
        }
    }
    
    assert!(found_manifest, "Archive should contain manifest.txt");
    assert!(found_router_yaml, "Archive should contain router.yaml");
    assert!(found_memory_dir, "Archive should contain memory/ directory");
    assert!(found_binary, "Archive should contain router binary (placeholder in test)");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_create_main_manifest() {
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: "/tmp/test-diagnostics".to_string(),
    };

    let result = ExportService::create_main_manifest(&config);
    assert!(result.is_ok(), "Manifest creation should succeed");
    
    let manifest_data = result.unwrap();
    let manifest_str = String::from_utf8(manifest_data).expect("Manifest should be valid UTF-8");
    
    // Check that manifest contains expected content
    assert!(manifest_str.contains("APOLLO ROUTER DIAGNOSTIC ARCHIVE"), "Should contain title");
    assert!(manifest_str.contains("Router Version:"), "Should contain version info");
    assert!(manifest_str.contains("Platform: linux"), "Should contain platform info");
    assert!(manifest_str.contains("Memory Output Directory: /tmp/test-diagnostics"), "Should contain output directory");
    assert!(manifest_str.contains("memory/"), "Should mention memory directory");
    assert!(manifest_str.contains("Memory Profiling: Enabled"), "Should mention memory profiling");
    assert!(manifest_str.contains("jemalloc profiling"), "Should mention jemalloc");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_add_router_binary_test_mode() {
    use std::io::Cursor;
    
    // Create a tar builder
    let mut archive_buffer = Vec::new();
    let cursor = Cursor::new(&mut archive_buffer);
    let mut tar = tar::Builder::new(cursor);
    
    // Add router binary (should be placeholder in test mode)
    let result = ExportService::add_router_binary(&mut tar);
    assert!(result.is_ok(), "Adding router binary should succeed");
    
    // Finish the archive
    let _inner = tar.into_inner().expect("Should be able to finish tar");
    
    // Verify that the archive contains the placeholder
    assert!(!archive_buffer.is_empty(), "Archive buffer should not be empty");
    
    // Parse the archive to verify contents
    let cursor = Cursor::new(archive_buffer);
    let mut archive = tar::Archive::new(cursor);
    let entries: Vec<_> = archive.entries()
        .expect("Should be able to read entries")
        .collect::<Result<Vec<_>, _>>()
        .expect("Should be able to collect entries");
    
    assert_eq!(entries.len(), 1, "Should have exactly one entry");
    
    let entry = &entries[0];
    let path = entry.path().expect("Should have path").to_string_lossy().to_string();
    assert_eq!(path, "router-binary.txt", "Should be the placeholder file");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_archive_with_empty_output_directory() {
    // Use a non-existent directory
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: "/tmp/nonexistent-diagnostics-dir".to_string(),
    };

    // Ensure the directory doesn't exist
    if Path::new(&config.output_directory).exists() {
        fs::remove_dir_all(&config.output_directory).ok();
    }

    let test_full_config = Some(serde_json::json!({"test": "config"}));
    let result = ExportService::create_archive(&config, &test_full_config);
    assert!(result.is_ok(), "Archive creation should succeed even with empty output directory");
    
    let archive_data = result.unwrap();
    assert!(!archive_data.is_empty(), "Archive should not be empty");
    
    // Verify the archive still contains expected structure
    let cursor = std::io::Cursor::new(archive_data);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);
    
    let mut entries = archive.entries().expect("Should be able to read archive entries");
    let mut found_manifest = false;
    let mut found_memory_dir = false;
    
    while let Some(entry) = entries.next() {
        let entry = entry.expect("Should be able to read entry");
        let path = entry.path().expect("Should have path").to_string_lossy().to_string();
        
        match path.as_str() {
            "manifest.txt" => found_manifest = true,
            "memory/" => found_memory_dir = true, // Empty directory should still be created
            _ => {} // Other files are okay
        }
    }
    
    assert!(found_manifest, "Archive should contain manifest.txt");
    assert!(found_memory_dir, "Archive should contain empty memory/ directory");
}

#[cfg(target_os = "linux")]
#[test]
fn test_create_comprehensive_archive_sync() {
    // Synchronous test for archive creation without tokio context
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: output_path,
    };

    let test_full_config = Some(serde_json::json!({"test": "config"}));
    let result = ExportService::create_archive(&config, &test_full_config);
    assert!(result.is_ok(), "Sync archive creation should succeed");
    
    let archive_data = result.unwrap();
    assert!(!archive_data.is_empty(), "Archive should not be empty");
    assert!(archive_data.len() > 100, "Archive should have substantial content");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_tar_gz_format_compatibility() {
    // This test verifies that our archives are proper tar.gz format
    // that can be extracted with standard tools
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    // Create test files in memory subdirectory
    let memory_path = format!("{}/memory", output_path);
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(format!("{}/heap_dump_1.prof", memory_path), b"heap dump content 1")
        .expect("Failed to write test file");
    fs::write(format!("{}/heap_dump_2.prof", memory_path), b"heap dump content 2")
        .expect("Failed to write test file");
    
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: output_path,
    };

    // Create the archive with test configuration
    let test_full_config = Some(serde_json::json!({
        "server": {
            "listen": "127.0.0.1:4000"
        },
        "experimental_diagnostics": {
            "enabled": true,
            "shared_secret": "test-secret"
        }
    }));
    let archive_data = ExportService::create_archive(&config, &test_full_config).expect("Archive creation should succeed");
    
    // Create extraction directory
    let extract_dir = tempdir().expect("Failed to create extraction dir");
    let extract_path = extract_dir.path();
    
    // Write archive to file and extract it using standard tar/gzip tools
    let archive_file = extract_path.join("test-archive.tar.gz");
    fs::write(&archive_file, &archive_data).expect("Failed to write archive file");
    
    // Extract using tar::Archive (simulating standard tar.gz extraction)
    let file = fs::File::open(&archive_file).expect("Failed to open archive file");
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    
    // Extract all files
    archive.unpack(extract_path).expect("Failed to extract archive");
    
    // Verify extracted contents
    let manifest_path = extract_path.join("manifest.txt");
    assert!(manifest_path.exists(), "Extracted manifest should exist");
    let manifest_content = fs::read_to_string(&manifest_path).expect("Failed to read manifest");
    assert!(manifest_content.contains("APOLLO ROUTER DIAGNOSTIC ARCHIVE"), "Manifest should have correct content");
    
    // Verify router.yaml was extracted and contains expected content
    let router_yaml_path = extract_path.join("router.yaml");
    assert!(router_yaml_path.exists(), "Extracted router.yaml should exist");
    let router_yaml_content = fs::read_to_string(&router_yaml_path).expect("Failed to read router.yaml");
    assert!(router_yaml_content.contains("server"), "router.yaml should contain configuration data");
    assert!(router_yaml_content.contains("experimental_diagnostics"), "router.yaml should contain diagnostics config");
    
    let memory_dir = extract_path.join("memory");
    assert!(memory_dir.exists(), "Extracted memory directory should exist");
    
    let heap_dump_1 = memory_dir.join("heap_dump_1.prof");
    assert!(heap_dump_1.exists(), "First heap dump should exist");
    let content_1 = fs::read(&heap_dump_1).expect("Failed to read heap dump 1");
    assert_eq!(content_1, b"heap dump content 1", "Heap dump content should match");
    
    let heap_dump_2 = memory_dir.join("heap_dump_2.prof");
    assert!(heap_dump_2.exists(), "Second heap dump should exist");
    let content_2 = fs::read(&heap_dump_2).expect("Failed to read heap dump 2");
    assert_eq!(content_2, b"heap dump content 2", "Heap dump content should match");
    
    let binary_file = extract_path.join("router-binary.txt"); // Placeholder in test mode
    assert!(binary_file.exists(), "Router binary placeholder should exist");
    
    // Verify that the archive is a valid gzip file by checking magic bytes
    assert!(archive_data.len() > 2, "Archive should be large enough for headers");
    assert_eq!(&archive_data[0..2], &[0x1f, 0x8b], "Should have gzip magic bytes");
}

#[cfg(target_os = "linux")]
#[test] 
fn test_manual_archive_inspection() {
    // Manual test to debug archive issues - outputs to /tmp for inspection
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    // Create some test data
    let memory_path = format!("{}/memory", output_path);
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(format!("{}/test.prof", memory_path), b"test profile data").expect("Failed to write test file");
    
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: output_path,
    };

    let test_full_config = Some(serde_json::json!({
        "server": {"listen": "127.0.0.1:4000"},
        "experimental_diagnostics": {"enabled": true, "shared_secret": "test-secret"}
    }));
    let archive_data = ExportService::create_archive(&config, &test_full_config).expect("Archive creation should succeed");
    
    // Write to /tmp for manual inspection
    let debug_archive = "/tmp/debug_router_diagnostics.tar.gz";
    fs::write(debug_archive, &archive_data).expect("Failed to write debug archive");
    println!("Debug archive written to: {}", debug_archive);
    
    // Verify with multiple tools
    let tar_list = std::process::Command::new("tar")
        .args(&["-tzf", debug_archive])
        .output()
        .expect("Failed to run tar");
        
    println!("Tar list output: {}", String::from_utf8_lossy(&tar_list.stdout));
    println!("Tar list stderr: {}", String::from_utf8_lossy(&tar_list.stderr));
    assert!(tar_list.status.success(), "tar -t should work");
    
    // Try to extract to verify completeness
    let extract_dir = "/tmp/debug_extract";
    let _ = fs::remove_dir_all(extract_dir); // Clean up any previous run
    fs::create_dir_all(extract_dir).expect("Failed to create extract dir");
    
    let tar_extract = std::process::Command::new("tar")
        .args(&["-xzf", debug_archive, "-C", extract_dir])
        .output()
        .expect("Failed to run tar extract");
        
    println!("Tar extract stderr: {}", String::from_utf8_lossy(&tar_extract.stderr));
    assert!(tar_extract.status.success(), "tar -x should work");
    
    // Verify extracted files exist
    assert!(Path::new(&format!("{}/manifest.txt", extract_dir)).exists());
    assert!(Path::new(&format!("{}/router.yaml", extract_dir)).exists());
    assert!(Path::new(&format!("{}/memory", extract_dir)).exists());
    
    // Print router.yaml content for verification
    let router_yaml_content = fs::read_to_string(&format!("{}/router.yaml", extract_dir))
        .expect("Failed to read router.yaml");
    println!("Router.yaml content:\n{}", router_yaml_content);
}

#[cfg(target_os = "linux")]
#[test] 
fn test_archive_format_with_system_tar() {
    // Test that verifies our archives work with system tar command
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: output_path,
    };

    let test_full_config = Some(serde_json::json!({
        "server": {"listen": "127.0.0.1:4000"},
        "experimental_diagnostics": {"enabled": true}
    }));
    let archive_data = ExportService::create_archive(&config, &test_full_config).expect("Archive creation should succeed");
    
    // Write archive to a file and test with system tar
    let archive_file = temp_dir.path().join("test.tar.gz");
    fs::write(&archive_file, &archive_data).expect("Failed to write archive");
    
    // Try to list contents with tar command
    let output = std::process::Command::new("tar")
        .args(&["-tzf", archive_file.to_str().unwrap()])
        .output()
        .expect("Failed to run tar command");
    
    assert!(output.status.success(), "tar command should succeed: stderr: {}", String::from_utf8_lossy(&output.stderr));
    
    let contents = String::from_utf8(output.stdout).expect("tar output should be valid UTF-8");
    assert!(contents.contains("manifest.txt"), "Archive should contain manifest.txt");
    assert!(contents.contains("router.yaml"), "Archive should contain router.yaml");
    assert!(contents.contains("memory/"), "Archive should contain memory directory");
}

#[cfg(target_os = "linux")]
#[test]
fn test_tar_gz_structure_validation() {
    // Test that ensures our tar.gz has the correct internal structure
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();
    
    let config = Config {
        enabled: true,
        listen: default_diagnostics_listen(),
        shared_secret: "test-secret".to_string(),
        output_directory: output_path,
    };

    let test_full_config = Some(serde_json::json!({"test": "configuration"}));
    let archive_data = ExportService::create_archive(&config, &test_full_config).expect("Archive creation should succeed");
    
    // Parse the tar.gz structure
    let cursor = std::io::Cursor::new(&archive_data);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);
    
    let entries: Vec<_> = archive.entries()
        .expect("Should be able to read entries")
        .collect::<Result<Vec<_>, _>>()
        .expect("Should be able to collect all entries");
    
    // Verify minimum expected structure
    assert!(!entries.is_empty(), "Archive should contain entries");
    
    let paths: Vec<String> = entries.iter()
        .map(|entry| entry.path().unwrap().to_string_lossy().to_string())
        .collect();
    
    // Check for required files/directories
    assert!(paths.contains(&"manifest.txt".to_string()), "Should contain manifest.txt");
    assert!(paths.contains(&"router.yaml".to_string()), "Should contain router.yaml");
    assert!(paths.iter().any(|p| p == "memory/" || p.starts_with("memory/")), "Should contain memory directory or files");
    assert!(paths.iter().any(|p| p.contains("router-binary")), "Should contain router binary or placeholder");
    
    // Verify file metadata
    for entry in entries {
        let path = entry.path().unwrap().to_string_lossy().to_string();
        let header = entry.header();
        
        // Verify proper file permissions
        if path == "manifest.txt" || path.ends_with(".prof") || path.contains("router-binary") {
            assert_ne!(header.mode().unwrap(), 0, "Files should have proper permissions");
        }
        
        // Verify proper entry types
        if path.ends_with("/") {
            assert!(header.entry_type().is_dir(), "Directories should be marked as directories");
        } else if !header.entry_type().is_dir() {
            assert!(header.entry_type().is_file(), "Non-directories should be files");
        }
    }
}