//! Tests for the export module

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;

use async_compression::tokio::bufread::GzipDecoder;
use bytes::Bytes;
use futures::StreamExt;
use futures::TryStreamExt;
use tempfile::tempdir;
use tokio::io::BufReader;
use tokio_tar::Archive;

use super::*;

/// Helper function to collect streaming archive data into a Vec<u8>
async fn collect_streaming_archive(
    config: &Config,
    supergraph_schema: &str,
    router_config: &serde_json::Value,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let stream = Exporter::create_streaming_archive(
        config.clone(),
        Arc::new(supergraph_schema.to_string()),
        Arc::new(router_config.to_string()),
    )
    .await;

    let chunks: Vec<Bytes> = stream
        .map(|chunk| chunk.expect("Stream should not fail"))
        .collect()
        .await;

    Ok(chunks.into_iter().fold(Vec::new(), |mut acc, chunk| {
        acc.extend_from_slice(&chunk);
        acc
    }))
}

#[tokio::test]
async fn test_export_service_creation() {
    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: "/tmp/router-diagnostics".to_string(),
    };

    let test_config_json = Arc::new(serde_json::json!({
        "test": "configuration"
    }));
    let export_service = Exporter::new(
        config.clone(),
        Arc::new("supergraph_schema".to_string()),
        Arc::new(test_config_json.to_string()),
    );
    assert_eq!(
        export_service.config().output_directory,
        config.output_directory
    );
}

#[tokio::test]
async fn test_create_archive() {
    // Create a temporary directory for test files
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    // On Linux, create memory files; on other platforms, the directory will be handled by the archive function
    #[cfg(target_family = "unix")]
    {
        let memory_path = format!("{}/memory", output_path);
        fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
        fs::write(
            format!("{}/test_heap.prof", memory_path),
            b"test heap dump data",
        )
        .expect("Failed to write test file");
        fs::write(
            format!("{}/test_profile.prof", memory_path),
            b"test profile data",
        )
        .expect("Failed to write another test file");
    }

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    // Create the archive with test configuration
    let test_full_config = serde_json::json!({
        "experimental_diagnostics": {
            "enabled": true,
            "shared_secret": "test-secret"
        }
    });
    let archive_data =
        collect_streaming_archive(&config, "test supergraph schema content", &test_full_config)
            .await
            .expect("Archive creation should succeed");
    assert!(!archive_data.is_empty(), "Archive should not be empty");

    // Verify the archive can be decompressed and read
    let reader = BufReader::new(archive_data.as_slice());
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    let mut entries = archive
        .entries()
        .expect("Should be able to read archive entries");
    let mut found_manifest = false;
    let mut found_router_yaml = false;
    let mut found_supergraph = false;
    let mut found_memory_content = false;
    // Router binary no longer included - enhanced profiles have embedded symbols
    let mut found_system_info = false;
    let mut found_html_report = false;

    while let Some(entry) = entries.try_next().await.expect("Should read entry") {
        let path = entry
            .path()
            .expect("Should have path")
            .to_string_lossy()
            .to_string();

        match path.as_str() {
            "manifest.txt" => found_manifest = true,
            "router.yaml" => found_router_yaml = true,
            "supergraph.graphql" => found_supergraph = true,
            "system_info.txt" => found_system_info = true,
            "diagnostics_report.html" => found_html_report = true,
            // Router binary no longer included
            path if path.starts_with("memory/") => found_memory_content = true,
            _ => {} // Other files are okay
        }
    }

    assert!(found_manifest, "Archive should contain manifest.txt");
    assert!(found_router_yaml, "Archive should contain router.yaml");
    assert!(
        found_supergraph,
        "Archive should contain supergraph.graphql"
    );
    assert!(found_system_info, "Archive should contain system_info.txt");
    assert!(
        found_memory_content,
        "Archive should contain memory/ content (files on Linux, README on other platforms)"
    );
    assert!(
        found_html_report,
        "Archive should contain diagnostics_report.html"
    );
    // Router binary no longer included - enhanced profiles have embedded symbols
}

#[tokio::test]
async fn test_create_main_manifest() {
    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: "/tmp/test-diagnostics".to_string(),
    };

    let result = Exporter::create_main_manifest(&config);
    assert!(result.is_ok(), "Manifest creation should succeed");

    let manifest_data = result.unwrap();
    let manifest_str = String::from_utf8(manifest_data).expect("Manifest should be valid UTF-8");

    // Check that manifest contains expected content
    assert!(
        manifest_str.contains("APOLLO ROUTER DIAGNOSTIC ARCHIVE"),
        "Should contain title"
    );
    assert!(
        manifest_str.contains("Router Version:"),
        "Should contain version info"
    );
    assert!(
        manifest_str.contains(&format!("Platform: {}", std::env::consts::OS)),
        "Should contain current platform info"
    );
    assert!(
        manifest_str.contains("Memory Output Directory: /tmp/test-diagnostics"),
        "Should contain output directory"
    );
    assert!(
        manifest_str.contains("supergraph.graphql"),
        "Should mention supergraph.graphql file"
    );
    assert!(
        manifest_str.contains("system_info.txt"),
        "Should mention system_info.txt file"
    );
    assert!(
        manifest_str.contains("memory/"),
        "Should mention memory directory or info"
    );
    assert!(
        manifest_str.contains("diagnostics_report.html"),
        "Should mention diagnostics_report.html"
    );

    // Platform-specific checks
    #[cfg(target_family = "unix")]
    {
        assert!(
            manifest_str.contains("Memory Profiling: Enabled"),
            "Should mention memory profiling enabled on Linux"
        );
        assert!(
            manifest_str.contains("jemalloc profiling"),
            "Should mention jemalloc on Linux"
        );
    }
    #[cfg(not(target_family = "unix"))]
    {
        assert!(
            manifest_str.contains("Memory Profiling: Not available"),
            "Should mention memory profiling not available on non-Linux"
        );
        assert!(
            manifest_str.contains("Heap dumps require Linux platform"),
            "Should mention Linux requirement on non-Linux platforms"
        );
    }
}

// Test for router binary removed - enhanced profiles now have embedded symbols

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_archive_with_empty_output_directory() {
    // Use a non-existent directory
    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),

        output_directory: "/tmp/nonexistent-diagnostics-dir".to_string(),
    };

    // Ensure the directory doesn't exist
    if Path::new(&config.output_directory).exists() {
        fs::remove_dir_all(&config.output_directory).ok();
    }

    let test_full_config = serde_json::json!({"test": "config"});
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed even with empty output directory");
    assert!(!archive_data.is_empty(), "Archive should not be empty");

    // Verify the archive still contains expected structure
    let reader = BufReader::new(archive_data.as_slice());
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    let mut entries = archive
        .entries()
        .expect("Should be able to read archive entries");
    let mut found_manifest = false;
    let mut found_memory_dir = false;

    while let Some(entry) = entries
        .try_next()
        .await
        .expect("Should be able to read entry")
    {
        let path = entry
            .path()
            .expect("Should have path")
            .to_string_lossy()
            .to_string();

        match path.as_str() {
            "manifest.txt" => found_manifest = true,
            "memory/" => found_memory_dir = true, // Empty directory should still be created
            _ => {}                               // Other files are okay
        }
    }

    assert!(found_manifest, "Archive should contain manifest.txt");
    assert!(
        found_memory_dir,
        "Archive should contain empty memory/ directory"
    );
}

#[tokio::test]
async fn test_create_archive_sync() {
    // Test for archive creation - works on all platforms
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({"test": "config"});
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");
    assert!(!archive_data.is_empty(), "Archive should not be empty");
    assert!(
        archive_data.len() > 100,
        "Archive should have substantial content"
    );
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_tar_gz_format_compatibility() {
    // This test verifies that our archives are proper tar.gz format
    // that can be extracted with standard tools
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    // Create test files in memory subdirectory
    let memory_path = format!("{}/memory", output_path);
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(
        format!("{}/heap_dump_1.prof", memory_path),
        b"heap dump content 1",
    )
    .expect("Failed to write test file");
    fs::write(
        format!("{}/heap_dump_2.prof", memory_path),
        b"heap dump content 2",
    )
    .expect("Failed to write test file");

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    // Create the archive with test configuration
    let test_full_config = serde_json::json!({
        "server": {
            "listen": "127.0.0.1:4000"
        },
        "experimental_diagnostics": {
            "enabled": true,
            "shared_secret": "test-secret"
        }
    });
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");

    // Create extraction directory
    let extract_dir = tempdir().expect("Failed to create extraction dir");
    let extract_path = extract_dir.path();

    // Write archive to file and extract it using standard tar/gzip tools
    let archive_file = extract_path.join("test-archive.tar.gz");
    fs::write(&archive_file, &archive_data).expect("Failed to write archive file");

    // Extract using tar::Archive (simulating standard tar.gz extraction)
    let file = tokio::fs::File::open(&archive_file)
        .await
        .expect("Failed to open archive file");
    let reader = BufReader::new(file);
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    // Extract all files
    archive
        .unpack(extract_path)
        .await
        .expect("Failed to extract archive");

    // Verify extracted contents
    let manifest_path = extract_path.join("manifest.txt");
    assert!(manifest_path.exists(), "Extracted manifest should exist");
    let manifest_content = fs::read_to_string(&manifest_path).expect("Failed to read manifest");
    assert!(
        manifest_content.contains("APOLLO ROUTER DIAGNOSTIC ARCHIVE"),
        "Manifest should have correct content"
    );

    // Verify router.yaml was extracted and contains expected content
    let router_yaml_path = extract_path.join("router.yaml");
    assert!(
        router_yaml_path.exists(),
        "Extracted router.yaml should exist"
    );
    let router_yaml_content =
        fs::read_to_string(&router_yaml_path).expect("Failed to read router.yaml");
    assert!(
        router_yaml_content.contains("server"),
        "router.yaml should contain configuration data"
    );
    assert!(
        router_yaml_content.contains("experimental_diagnostics"),
        "router.yaml should contain diagnostics config"
    );

    // Verify supergraph.graphql was extracted and contains expected content
    let supergraph_path = extract_path.join("supergraph.graphql");
    assert!(
        supergraph_path.exists(),
        "Extracted supergraph.graphql should exist"
    );
    let supergraph_content =
        fs::read_to_string(&supergraph_path).expect("Failed to read supergraph.graphql");
    assert_eq!(
        supergraph_content, "supergraph_schema",
        "supergraph.graphql should contain the schema content"
    );

    let memory_dir = extract_path.join("memory");
    assert!(
        memory_dir.exists(),
        "Extracted memory directory should exist"
    );

    let heap_dump_1 = memory_dir.join("heap_dump_1.prof");
    assert!(heap_dump_1.exists(), "First heap dump should exist");
    let content_1 = fs::read(&heap_dump_1).expect("Failed to read heap dump 1");
    assert_eq!(
        content_1, b"heap dump content 1",
        "Heap dump content should match"
    );

    let heap_dump_2 = memory_dir.join("heap_dump_2.prof");
    assert!(heap_dump_2.exists(), "Second heap dump should exist");
    let content_2 = fs::read(&heap_dump_2).expect("Failed to read heap dump 2");
    assert_eq!(
        content_2, b"heap dump content 2",
        "Heap dump content should match"
    );

    // Router binary no longer included - enhanced profiles have embedded symbols

    // Verify that the archive is a valid gzip file by checking magic bytes
    assert!(
        archive_data.len() > 2,
        "Archive should be large enough for headers"
    );
    assert_eq!(
        &archive_data[0..2],
        &[0x1f, 0x8b],
        "Should have gzip magic bytes"
    );
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_manual_archive_inspection() {
    // Manual test to debug archive issues - outputs to /tmp for inspection
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    // Create some test data
    let memory_path = format!("{}/memory", output_path);
    fs::create_dir_all(&memory_path).expect("Failed to create memory directory");
    fs::write(format!("{}/test.prof", memory_path), b"test profile data")
        .expect("Failed to write test file");

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({
        "server": {"listen": "127.0.0.1:4000"},
        "experimental_diagnostics": {"enabled": true, "shared_secret": "test-secret"}
    });
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");

    // Write to /tmp for manual inspection
    let debug_archive = "/tmp/debug_router_diagnostics.tar.gz";
    fs::write(debug_archive, &archive_data).expect("Failed to write debug archive");
    tracing::debug!("Debug archive written to: {}", debug_archive);

    // Verify with multiple tools
    let tar_list = std::process::Command::new("tar")
        .args(["-tzf", debug_archive])
        .output()
        .expect("Failed to run tar");

    tracing::debug!(
        "Tar list output: {}",
        String::from_utf8_lossy(&tar_list.stdout)
    );
    tracing::debug!(
        "Tar list stderr: {}",
        String::from_utf8_lossy(&tar_list.stderr)
    );
    assert!(tar_list.status.success(), "tar -t should work");

    // Try to extract to verify completeness
    let extract_dir = "/tmp/debug_extract";
    let _ = fs::remove_dir_all(extract_dir); // Clean up any previous run
    fs::create_dir_all(extract_dir).expect("Failed to create extract dir");

    let tar_extract = std::process::Command::new("tar")
        .args(["-xzf", debug_archive, "-C", extract_dir])
        .output()
        .expect("Failed to run tar extract");

    tracing::debug!(
        "Tar extract stderr: {}",
        String::from_utf8_lossy(&tar_extract.stderr)
    );
    assert!(tar_extract.status.success(), "tar -x should work");

    // Verify extracted files exist
    assert!(Path::new(&format!("{}/manifest.txt", extract_dir)).exists());
    assert!(Path::new(&format!("{}/router.yaml", extract_dir)).exists());
    assert!(Path::new(&format!("{}/memory", extract_dir)).exists());

    // Log router.yaml content for verification (debug only)
    let router_yaml_content = fs::read_to_string(format!("{extract_dir}/router.yaml"))
        .expect("Failed to read router.yaml");
    tracing::debug!("Router.yaml content:\n{}", router_yaml_content);
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_archive_format_with_system_tar() {
    // Test that verifies our archives work with system tar command
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({
        "server": {"listen": "127.0.0.1:4000"},
        "experimental_diagnostics": {"enabled": true}
    });
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");

    // Write archive to a file and test with system tar
    let archive_file = temp_dir.path().join("test.tar.gz");
    fs::write(&archive_file, &archive_data).expect("Failed to write archive");

    // Try to list contents with tar command
    let output = std::process::Command::new("tar")
        .args(["-tzf", archive_file.to_str().unwrap()])
        .output()
        .expect("Failed to run tar command");

    assert!(
        output.status.success(),
        "tar command should succeed: stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contents = String::from_utf8(output.stdout).expect("tar output should be valid UTF-8");
    assert!(
        contents.contains("manifest.txt"),
        "Archive should contain manifest.txt"
    );
    assert!(
        contents.contains("router.yaml"),
        "Archive should contain router.yaml"
    );
    assert!(
        contents.contains("supergraph.graphql"),
        "Archive should contain supergraph.graphql"
    );
    assert!(
        contents.contains("memory/"),
        "Archive should contain memory directory"
    );
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn test_tar_gz_structure_validation() {
    // Test that ensures our tar.gz has the correct internal structure
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({"test": "configuration"});
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");

    // Parse the tar.gz structure
    let reader = BufReader::new(archive_data.as_slice());
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    let mut entries_stream = archive.entries().expect("Should be able to read entries");

    let mut entries = Vec::new();
    while let Some(entry) = entries_stream
        .try_next()
        .await
        .expect("Should be able to read entry")
    {
        entries.push(entry);
    }

    // Verify minimum expected structure
    assert!(!entries.is_empty(), "Archive should contain entries");

    let paths: Vec<String> = entries
        .iter()
        .map(|entry: &tokio_tar::Entry<_>| entry.path().unwrap().to_string_lossy().to_string())
        .collect();

    // Check for required files/directories
    assert!(
        paths.contains(&"manifest.txt".to_string()),
        "Should contain manifest.txt"
    );
    assert!(
        paths.contains(&"router.yaml".to_string()),
        "Should contain router.yaml"
    );
    assert!(
        paths.contains(&"supergraph.graphql".to_string()),
        "Should contain supergraph.graphql"
    );
    assert!(
        paths
            .iter()
            .any(|p| p == "memory/" || p.starts_with("memory/")),
        "Should contain memory directory or files"
    );
    // Router binary no longer included - enhanced profiles have embedded symbols

    // Verify file metadata
    for entry in entries {
        let path = entry.path().unwrap().to_string_lossy().to_string();
        let header = entry.header();

        // Verify proper file permissions
        if path == "manifest.txt" || path.ends_with(".prof") || path.contains("router") {
            assert_ne!(
                header.mode().unwrap(),
                0,
                "Files should have proper permissions"
            );
        }

        // Verify proper entry types
        if path.ends_with("/") {
            assert!(
                header.entry_type().is_dir(),
                "Directories should be marked as directories"
            );
        } else if !header.entry_type().is_dir() {
            assert!(
                header.entry_type().is_file(),
                "Non-directories should be files"
            );
        }
    }
}

#[tokio::test]
async fn test_non_unix_memory_handling() {
    // Test specifically for non-Linux platforms to ensure they get the README file
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({"test": "config"});
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");
    let reader = BufReader::new(archive_data.as_slice());
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    let mut entries = archive
        .entries()
        .expect("Should be able to read archive entries");

    let mut found_memory_content = false;
    while let Some(entry) = entries
        .try_next()
        .await
        .expect("Should be able to read entry")
    {
        let path = entry
            .path()
            .expect("Should have path")
            .to_string_lossy()
            .to_string();

        if path.starts_with("memory/") {
            found_memory_content = true;
            #[cfg(target_family = "unix")]
            {
                // On Linux, we might have actual profile files if they exist
                assert!(
                    path.ends_with(".prof") || path == "memory/" || path == "memory/README.txt",
                    "Memory files should be profile files or directories on Linux"
                );
            }
            #[cfg(not(target_family = "unix"))]
            {
                // On non-Linux, we should only have the README
                assert_eq!(
                    path, "memory/README.txt",
                    "Non-Linux should only have README in memory directory"
                );
            }
        }
    }

    assert!(
        found_memory_content,
        "Should have some memory-related content"
    );
}

#[tokio::test]
async fn test_system_info_collection() {
    // Test that system information collection works on all platforms
    let result = super::super::system_info::collect().await;
    assert!(result.is_ok(), "System info collection should succeed");

    let system_info = result.unwrap();
    assert!(!system_info.is_empty(), "System info should not be empty");

    // Check for required sections that should exist on all platforms
    assert!(
        system_info.contains("SYSTEM INFORMATION"),
        "Should contain system information header"
    );
    assert!(
        system_info.contains("Operating System:"),
        "Should contain OS information"
    );
    assert!(
        system_info.contains("Architecture:"),
        "Should contain architecture information"
    );
    assert!(
        system_info.contains("Process ID:"),
        "Should contain process ID"
    );
    assert!(
        system_info.contains("Router Version:"),
        "Should contain router version"
    );
    assert!(
        system_info.contains("MEMORY INFORMATION"),
        "Should contain memory information section"
    );
    assert!(
        system_info.contains("CPU INFORMATION"),
        "Should contain CPU information section"
    );
    assert!(
        system_info.contains("Physical CPU cores:"),
        "Should contain CPU core count"
    );
    assert!(
        system_info.contains("RELEVANT ENVIRONMENT VARIABLES"),
        "Should contain environment variables section"
    );

    // Platform-specific checks
    #[cfg(target_family = "unix")]
    {
        // On Linux, we might have detailed memory and CPU info
        // But we don't assert on these as they depend on system state
        // Just verify the structure is there
        assert!(
            system_info.contains("------------------"),
            "Should have section separators"
        );
    }

    #[cfg(not(target_family = "unix"))]
    {
        // On non-Linux platforms, we should have fallback messages
        assert!(
            system_info.contains("not available") || system_info.contains("Unknown"),
            "Should have appropriate fallback messages for unavailable info"
        );
    }
}

#[tokio::test]
async fn test_system_info_in_archive_extraction() {
    // Test that system info is properly included in archive and can be extracted
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({"test": "config"});
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");

    // Extract and verify system_info.txt
    let extract_dir = tempdir().expect("Failed to create extraction dir");
    let extract_path = extract_dir.path();

    let reader = BufReader::new(archive_data.as_slice());
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    archive
        .unpack(extract_path)
        .await
        .expect("Failed to extract archive");

    // Verify system_info.txt was extracted and contains expected content
    let system_info_path = extract_path.join("system_info.txt");
    assert!(
        system_info_path.exists(),
        "Extracted system_info.txt should exist"
    );

    let system_info_content =
        fs::read_to_string(&system_info_path).expect("Failed to read system_info.txt");

    // Verify basic structure exists
    assert!(
        system_info_content.contains("SYSTEM INFORMATION"),
        "system_info.txt should contain system information header"
    );
    assert!(
        system_info_content.contains(&format!("Operating System: {}", std::env::consts::OS)),
        "system_info.txt should contain correct OS information"
    );
    assert!(
        system_info_content.contains(&format!("Architecture: {}", std::env::consts::ARCH)),
        "system_info.txt should contain correct architecture information"
    );
    assert!(
        system_info_content.contains("Process ID:"),
        "system_info.txt should contain process ID"
    );

    // The content should be substantial (more than just headers)
    assert!(
        system_info_content.len() > 500,
        "system_info.txt should contain substantial information"
    );
}

#[tokio::test]
async fn test_system_info_environment_variables() {
    // Test that environment variable collection works properly
    // This test only runs when APOLLO_GRAPH_REF is set (e.g., in CI)
    if std::env::var("APOLLO_GRAPH_REF").is_ok() {
        let system_info = super::super::system_info::collect()
            .await
            .expect("Should collect system info");

        // Should have environment variables section
        assert!(
            system_info.contains("RELEVANT ENVIRONMENT VARIABLES"),
            "Should have environment variables section"
        );

        // Should contain the APOLLO_GRAPH_REF since it's set
        assert!(
            system_info.contains("APOLLO_GRAPH_REF:"),
            "Should contain APOLLO_GRAPH_REF when it's set"
        );

        // Should not contain the "no variables" message
        assert!(
            !system_info.contains("No relevant Apollo environment variables set"),
            "Should not show 'no variables' message when APOLLO_GRAPH_REF is set"
        );
    } else {
        // When APOLLO_GRAPH_REF is not set, test the fallback behavior
        let system_info = super::super::system_info::collect()
            .await
            .expect("Should collect system info");

        // Should have environment variables section
        assert!(
            system_info.contains("RELEVANT ENVIRONMENT VARIABLES"),
            "Should have environment variables section"
        );

        // Should indicate no relevant variables are set
        assert!(
            system_info.contains("No relevant Apollo environment variables set"),
            "Should indicate no relevant variables when APOLLO_GRAPH_REF is not set"
        );
    }
}

#[tokio::test]
async fn test_system_info_cpu_count() {
    // Test that CPU count detection works on all platforms
    let system_info = super::super::system_info::collect()
        .await
        .expect("Should collect system info");

    // Should contain CPU information
    assert!(
        system_info.contains("Physical CPU cores:"),
        "Should contain CPU core count information"
    );

    // The actual number should be at least 1
    let cpu_count = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1);
    assert!(cpu_count >= 1, "Should detect at least 1 CPU core");

    // Verify the detected count appears in the system info
    assert!(
        system_info.contains(&cpu_count.to_string()),
        "System info should contain the detected CPU count"
    );
}

#[tokio::test]
async fn test_html_report_in_archive() {
    // Test that diagnostics_report.html is included in the archive
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_path = temp_dir.path().to_str().unwrap().to_string();

    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: output_path,
    };

    let test_full_config = serde_json::json!({"test": "config"});
    let archive_data = collect_streaming_archive(&config, "supergraph_schema", &test_full_config)
        .await
        .expect("Archive creation should succeed");

    // Parse the archive to check for HTML report
    let reader = BufReader::new(archive_data.as_slice());
    let decoder = GzipDecoder::new(reader);
    let mut archive = Archive::new(decoder);

    let mut entries_stream = archive.entries().expect("Should be able to read entries");
    let mut found_html_report = false;

    while let Some(entry) = entries_stream
        .try_next()
        .await
        .expect("Should be able to read entry")
    {
        let path = entry.path().unwrap().to_string_lossy().to_string();
        if path == "diagnostics_report.html" {
            found_html_report = true;
            let header = entry.header();

            // Verify it's a file
            assert!(
                header.entry_type().is_file(),
                "diagnostics_report.html should be a regular file"
            );

            // Verify it has content (HTML should be substantial)
            assert!(
                header.size().unwrap() > 1000,
                "diagnostics_report.html should have substantial content"
            );
        }
    }

    assert!(
        found_html_report,
        "Archive should contain diagnostics_report.html"
    );
}
