//! Security validation utilities for diagnostics plugin
//!
//! This module provides security validation functions to prevent common
//! attacks such as path traversal and file type validation.

use displaydoc::Display;

use super::DiagnosticsError;

/// Security validation errors
#[derive(Debug, thiserror::Error, Display)]
pub(super) enum SecurityError {
    /// Path traversal attempt detected
    PathTraversal { filename: String },
    /// Invalid file type
    InvalidFileType {
        filename: String,
        allowed_extensions: Vec<String>,
    },
    /// File not found (prevents information disclosure)
    FileNotFound { filename: String },
}

// Convert SecurityError to DiagnosticsError
impl From<SecurityError> for DiagnosticsError {
    fn from(error: SecurityError) -> Self {
        DiagnosticsError::Internal(error.to_string())
    }
}

/// Security validator for file operations
pub(super) struct SecurityValidator;

impl SecurityValidator {
    /// Validate filename for path traversal attacks
    ///
    /// Checks for:
    /// - Parent directory references ".."
    /// - Forward slashes "/" (Unix path separators)
    /// - Backslashes "\" (Windows path separators)
    ///
    /// This prevents directory traversal attacks that could allow access
    /// to arbitrary files on the filesystem.
    pub(super) fn validate_filename(filename: &str) -> Result<(), SecurityError> {
        if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
            return Err(SecurityError::PathTraversal {
                filename: filename.to_string(),
            });
        }
        Ok(())
    }

    /// Validate file extension against allowed types
    ///
    /// This prevents access to sensitive files by restricting operations
    /// to specific file types (e.g., only .prof files for heap dumps).
    pub(super) fn validate_file_extension(
        filename: &str,
        allowed_extensions: &[&str],
    ) -> Result<(), SecurityError> {
        let has_valid_extension = allowed_extensions.iter().any(|ext| filename.ends_with(ext));

        if !has_valid_extension {
            return Err(SecurityError::InvalidFileType {
                filename: filename.to_string(),
                allowed_extensions: allowed_extensions.iter().map(|s| s.to_string()).collect(),
            });
        }
        Ok(())
    }

    /// Validate filename for memory dump operations
    ///
    /// Combines path traversal and file extension validation
    /// specifically for .prof files used by jemalloc.
    pub(super) fn validate_memory_dump_filename(filename: &str) -> Result<(), SecurityError> {
        Self::validate_filename(filename)?;
        Self::validate_file_extension(filename, &[".prof"])?;
        Ok(())
    }

    /// Check if file exists and validate it's not a directory
    ///
    /// This prevents information disclosure about filesystem structure
    /// by ensuring we only report file existence for valid files.
    pub(super) fn validate_file_exists_and_is_file<P: AsRef<std::path::Path>>(
        path: P,
        filename: &str,
    ) -> Result<(), SecurityError> {
        let path = path.as_ref();
        if !path.exists() || !path.is_file() {
            return Err(SecurityError::FileNotFound {
                filename: filename.to_string(),
            });
        }
        Ok(())
    }

    /// Comprehensive validation for file download operations
    ///
    /// Combines all security checks needed for safe file downloads:
    /// - Path traversal prevention
    /// - File type validation
    /// - File existence verification
    pub(super) fn validate_file_download(
        file_path: &std::path::Path,
        filename: &str,
        allowed_extensions: &[&str],
    ) -> Result<(), SecurityError> {
        Self::validate_filename(filename)?;
        Self::validate_file_extension(filename, allowed_extensions)?;
        Self::validate_file_exists_and_is_file(file_path, filename)?;
        Ok(())
    }

    /// Comprehensive validation for file deletion operations
    ///
    /// Same as download validation but specifically for delete operations
    /// where security is even more critical since it modifies the filesystem.
    pub(super) fn validate_file_deletion(
        file_path: &std::path::Path,
        filename: &str,
        allowed_extensions: &[&str],
    ) -> Result<(), SecurityError> {
        // Use same validation as download - deletion requires same security checks
        Self::validate_file_download(file_path, filename, allowed_extensions)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn test_validate_filename_safe() {
        assert!(SecurityValidator::validate_filename("test.prof").is_ok());
        assert!(SecurityValidator::validate_filename("router_heap_dump_1234567890.prof").is_ok());
        assert!(SecurityValidator::validate_filename("simple_file.txt").is_ok());
    }

    #[test]
    fn test_validate_filename_path_traversal() {
        assert!(SecurityValidator::validate_filename("../test.prof").is_err());
        assert!(SecurityValidator::validate_filename("test/../file.prof").is_err());
        assert!(SecurityValidator::validate_filename("..\\test.prof").is_err());
        assert!(SecurityValidator::validate_filename("path/to/file.prof").is_err());
        assert!(SecurityValidator::validate_filename("path\\to\\file.prof").is_err());
    }

    #[test]
    fn test_validate_file_extension() {
        assert!(SecurityValidator::validate_file_extension("test.prof", &[".prof"]).is_ok());
        assert!(SecurityValidator::validate_file_extension("test.txt", &[".txt", ".log"]).is_ok());

        assert!(SecurityValidator::validate_file_extension("test.exe", &[".prof"]).is_err());
        assert!(SecurityValidator::validate_file_extension("test.prof", &[".txt"]).is_err());
        assert!(SecurityValidator::validate_file_extension("test", &[".prof"]).is_err());
    }

    #[test]
    fn test_validate_memory_dump_filename() {
        assert!(
            SecurityValidator::validate_memory_dump_filename("router_heap_dump_1234.prof").is_ok()
        );

        assert!(SecurityValidator::validate_memory_dump_filename("../test.prof").is_err());
        assert!(SecurityValidator::validate_memory_dump_filename("test.exe").is_err());
        assert!(SecurityValidator::validate_memory_dump_filename("path/test.prof").is_err());
    }

    #[test]
    fn test_validate_file_exists_and_is_file() {
        // Test with non-existent file
        let nonexistent = Path::new("/path/that/does/not/exist/file.prof");
        assert!(
            SecurityValidator::validate_file_exists_and_is_file(nonexistent, "file.prof").is_err()
        );

        // Test with directory (if current directory exists)
        let current_dir = Path::new(".");
        if current_dir.exists() && current_dir.is_dir() {
            assert!(
                SecurityValidator::validate_file_exists_and_is_file(current_dir, "directory")
                    .is_err()
            );
        }

        // Create a temporary file for testing
        use std::fs::File;

        use tempfile::tempdir;

        let temp_dir = tempdir().expect("Failed to create temp dir");
        let temp_file = temp_dir.path().join("test.prof");
        File::create(&temp_file).expect("Failed to create temp file");

        // Test with existing file
        assert!(
            SecurityValidator::validate_file_exists_and_is_file(&temp_file, "test.prof").is_ok()
        );
    }

    #[test]
    fn test_security_error_types() {
        let path_traversal = SecurityError::PathTraversal {
            filename: "../test".to_string(),
        };
        let invalid_type = SecurityError::InvalidFileType {
            filename: "test.exe".to_string(),
            allowed_extensions: vec![".prof".to_string()],
        };
        let not_found = SecurityError::FileNotFound {
            filename: "missing.prof".to_string(),
        };

        // Just verify the errors can be created - response testing would require mock request
        assert!(matches!(
            path_traversal,
            SecurityError::PathTraversal { .. }
        ));
        assert!(matches!(
            invalid_type,
            SecurityError::InvalidFileType { .. }
        ));
        assert!(matches!(not_found, SecurityError::FileNotFound { .. }));
    }
}
