use crate::plugins::diagnostics::system_info;

#[tokio::test]
async fn test_system_info_collection() {
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();
    assert!(info.contains("SYSTEM INFORMATION"));
    assert!(info.contains("Operating System:"));
    assert!(info.contains("Architecture:"));
    assert!(info.contains("Router Version:"));

    // Test new normalized values are included
    assert!(info.contains("(") && info.contains(")")); // Should have normalized values in parentheses
}

#[tokio::test]
async fn test_system_info_cpu_count() {
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();
    // Should contain CPU information section
    assert!(info.contains("CPU INFORMATION"));
    // Should have new CPU info format
    assert!(info.contains("Physical CPU cores:"));
    assert!(info.contains("Available Parallelism:"));
    assert!(info.contains("Container/Kubernetes CPU Information:"));
}

#[tokio::test]
async fn test_system_info_environment_variables() {
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();
    // Should contain environment variables section
    assert!(info.contains("RELEVANT ENVIRONMENT VARIABLES"));
}

#[tokio::test]
async fn test_system_info_memory_details() {
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();
    // Should contain memory information with new format
    assert!(info.contains("MEMORY INFORMATION"));
    assert!(info.contains("Total Memory:"));
    assert!(info.contains("GB"));
    assert!(info.contains("bytes"));
}

#[tokio::test]
async fn test_system_info_container_detection() {
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();
    // Should contain container environment information
    assert!(info.contains("Container Environment:"));
    // Should have some form of detection result
    assert!(info.contains("detected") || info.contains("Not detected"));
}

#[test]
fn test_normalization_functions() {
    use super::get_normalized_arch;
    use super::get_normalized_os;

    // Test that normalization functions work
    let normalized_os = get_normalized_os();
    let normalized_arch = get_normalized_arch();

    // Should return valid strings
    assert!(!normalized_os.is_empty());
    assert!(!normalized_arch.is_empty());

    // Test specific mappings if on known platforms
    #[cfg(target_os = "linux")]
    assert_eq!(normalized_os, "linux");

    #[cfg(target_arch = "x86_64")]
    assert_eq!(normalized_arch, "amd64");

    #[cfg(target_arch = "aarch64")]
    assert_eq!(normalized_arch, "arm64");
}

#[tokio::test]
async fn test_system_info_cpu_load_collection() {
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();

    // Should contain system load information section
    assert!(info.contains("SYSTEM LOAD"));

    // Should contain either load average information or CPU usage fallback
    // Load average is available on Unix systems, CPU usage is fallback for others
    let has_load_average = info.contains("Load Average (1min):");
    let has_cpu_usage_fallback = info.contains("CPU Usage (per core):");
    let has_not_available = info.contains("Load Average: Not available");

    // At least one of these should be present
    assert!(
        has_load_average || has_cpu_usage_fallback || has_not_available,
        "Should have either load average, CPU usage fallback, or 'not available' message. Info contains: {}",
        info
    );

    // If load average is available, should also have load per CPU and individual core usage
    if has_load_average {
        assert!(info.contains("Load per CPU (1min):"));
        assert!(info.contains("utilization"));
        assert!(info.contains("Individual CPU Core Usage:"));
        assert!(info.contains("Average CPU Usage:"));
    }

    // If using CPU usage fallback, should have total average
    if has_cpu_usage_fallback {
        assert!(info.contains("Total CPU Usage (average):"));
    }
}

#[tokio::test]
async fn test_system_info_in_archive_extraction() {
    // This test verifies system info can be collected without errors
    // when running in different environments (like during archive extraction)
    let result = system_info::collect().await;
    assert!(result.is_ok());

    let info = result.unwrap();
    // Basic validation that we got some system information
    assert!(!info.is_empty());
    assert!(info.len() > 100); // Should have substantial content
}
