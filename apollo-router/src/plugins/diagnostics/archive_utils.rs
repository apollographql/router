//! Archive utility functions for diagnostics plugin
//!
//! This module provides utilities for creating tar archives with consistent
//! headers, timestamps, and error handling.

use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use super::DiagnosticsError;
use super::DiagnosticsResult;

/// Builder for creating tar archive headers with consistent settings
pub(super) struct ArchiveHeaderBuilder {
    path: String,
    mode: u32,
    timestamp: Option<u64>,
}

impl ArchiveHeaderBuilder {
    /// Create a new archive header builder for the given path
    pub(super) fn new<P: AsRef<str>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_string(),
            mode: 0o644, // Default file permissions
            timestamp: None,
        }
    }

    /// Set a custom timestamp (defaults to current time)
    pub(super) fn timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    /// Build a tar header for the given content
    pub(super) fn build_for_content(self, content: &[u8]) -> DiagnosticsResult<tokio_tar::Header> {
        let mut header = tokio_tar::Header::new_gnu();

        // Set path with error handling
        header.set_path(&self.path).map_err(|e| {
            DiagnosticsError::Internal(format!("Failed to set path '{}': {}", self.path, e))
        })?;

        // Set content size
        header.set_size(content.len() as u64);

        // Set file permissions
        header.set_mode(self.mode);

        // Set timestamp (current time if not specified)
        let timestamp = self.timestamp.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
        header.set_mtime(timestamp);

        // Set checksum
        header.set_cksum();

        Ok(header)
    }
}

/// Utility functions for adding content to tar archives
pub(super) struct ArchiveUtils;

impl ArchiveUtils {
    /// Add text content to a tar archive with standard header settings
    pub(super) async fn add_text_file<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        path: &str,
        content: &str,
    ) -> DiagnosticsResult<()> {
        let content_bytes = content.as_bytes();
        let header = ArchiveHeaderBuilder::new(path).build_for_content(content_bytes)?;

        tar.append(&header, content_bytes).await.map_err(|e| {
            DiagnosticsError::Internal(format!("Failed to add '{}' to archive: {}", path, e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_archive_header_builder_default() {
        let content = b"test content";
        let header = ArchiveHeaderBuilder::new("test.txt")
            .build_for_content(content)
            .unwrap();

        assert_eq!(header.path().unwrap().to_str().unwrap(), "test.txt");
        assert_eq!(header.size().unwrap(), content.len() as u64);
        assert_eq!(header.mode().unwrap(), 0o644);
        assert!(header.mtime().unwrap() > 0); // Should have a timestamp
    }

    #[test]
    fn test_archive_header_builder_custom_timestamp() {
        let content = b"test content";
        let custom_timestamp = 1234567890;

        let header = ArchiveHeaderBuilder::new("test.txt")
            .timestamp(custom_timestamp)
            .build_for_content(content)
            .unwrap();

        assert_eq!(header.mtime().unwrap(), custom_timestamp);
    }

    #[tokio::test]
    async fn test_archive_utils_add_text_file() {
        use std::io::Cursor;

        let buffer = Vec::new();
        let cursor = Cursor::new(buffer);
        let mut tar = tokio_tar::Builder::new(cursor);

        let result = ArchiveUtils::add_text_file(&mut tar, "test.txt", "Hello, World!").await;

        assert!(result.is_ok());
    }
}
