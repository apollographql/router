//! Export functionality for diagnostics plugin
//!
//! This module handles the creation of diagnostic archives containing data
//! from all diagnostic modules. It provides an export system
//! that can be extended by additional diagnostic modules.
//!
//! ## Streaming Architecture
//!
//! The diagnostic export uses streaming to handle potentially large archives (memory dumps) without
//! consuming excessive memory or blocking the HTTP response.
//!
//! ### Unidirectional Pipe Design
//!
//! The archive creation process uses a unidirectional pipe with different I/O patterns:
//!
//! ```text
//! [Tar Builder] -> [Gzip Encoder] -> [SimplexStream Writer] ====> [SimplexStream Reader] -> [HTTP Stream]
//!     ^                                        ^                            ^                      ^
//!  Push-based                            Push-based                  Pull-based              Pull-based
//!    AsyncWrite                         AsyncWrite                 AsyncRead               Stream/Body
//! ```
//!
//! **Push-based writers** (tar/gzip): Generate data and push it to the simplex writer via `AsyncWrite`.
//! They produce data as fast as the downstream consumer can handle it.
//!
//! **Pull-based consumers** (HTTP): Request data when ready by reading from the simplex reader.
//! The HTTP client determines the consumption rate based on network speed, processing, etc.
//!
//! **The Simplex Bridge**: `tokio::io::simplex()` creates a unidirectional in-memory pipe:
//! - Writer implements `AsyncWrite` for tar/gzip to push data
//! - Reader implements `AsyncRead` for HTTP streaming to pull data
//! - Built-in **backpressure** prevents memory exhaustion with bounded buffering
//!
//! ### Memory Safety & Backpressure
//!
//! Without backpressure, a fast producer (tar creation ~GB/s) and slow consumer
//! (network ~MB/s) would cause unbounded memory growth and potential OOM kills.
//!
//! **Backpressure mechanism:**
//! 1. **Bounded buffer**: SimplexStream has a 2MB internal buffer limit
//! 2. **AsyncWrite backpressure**: Returns `Poll::Pending` when buffer is full
//! 3. **Tar writer blocks**: Waits until HTTP client reads data and frees buffer space
//! 4. **Flow control**: Automatic rate matching between producer and consumer
//!
//! This ensures:
//! - ✅ Memory usage stays bounded regardless of network speed
//! - ✅ Large archives stream efficiently without blocking
//! - ✅ Automatic cancellation when HTTP client disconnects
//! - ✅ No temporary files or memory accumulation
//! - ✅ Optimal performance with unidirectional data flow
//!
//! **Platform Support**: This module is available on all platforms.
//! Memory heap dumps are only available on Unix platforms.

use std::sync::Arc;

use async_compression::tokio::write::GzipEncoder;
use axum::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use http::Response;
use http::StatusCode;
use http::header;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use tower::BoxError;

use super::Config;
use super::DiagnosticsError;
use super::DiagnosticsResult;
use super::archive_utils::ArchiveUtils;
use super::memory;

#[cfg(test)]
mod tests;

/// Export service responsible for creating diagnostic archives
#[derive(Debug, Clone)]
pub(super) struct Exporter {
    config: Config,
    supergraph_schema: Arc<String>,
    router_config: Arc<str>,
}

impl Exporter {
    /// Create a new export service with the given configuration
    pub(super) fn new(
        config: Config,
        supergraph_schema: Arc<String>,
        router_config: Arc<str>,
    ) -> Self {
        Self {
            config,
            supergraph_schema,
            router_config,
        }
    }

    /// Handle GET /diagnostics/export
    /// Creates a diagnostic archive by streaming data from all diagnostic modules
    pub(super) async fn export(self) -> Result<Response<Body>, DiagnosticsError> {
        tracing::info!("Diagnostic export requested");

        // Generate filename with current timestamp
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to get current timestamp: {}", e))
            })?
            .as_secs();
        let filename = format!("router-diagnostics-{}.tar.gz", timestamp);

        // Create streaming tar archive
        let data_stream =
            Self::create_streaming_archive(self.config, self.supergraph_schema, self.router_config)
                .await;

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/gzip")
            // Chunked encoding enables streaming without knowing total size upfront
            .header(header::TRANSFER_ENCODING, "chunked")
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            )
            .body(Body::from_stream(data_stream))
            .map_err(DiagnosticsError::Http)
    }

    /// Creates a streaming diagnostic archive that yields chunks as they're created
    ///
    /// This function uses a `tokio::io::simplex()` unidirectional pipe to efficiently
    /// stream archive data directly to the HTTP response without buffering the entire
    /// archive in memory. The archive creation runs in a separate task while data
    /// is streamed to the client with built-in backpressure control.
    async fn create_streaming_archive(
        config: Config,
        supergraph_schema: Arc<String>,
        router_config: Arc<str>,
    ) -> impl futures::Stream<Item = Result<Bytes, BoxError>> + Send + 'static {
        // Use tokio::io::simplex for unidirectional pipe with backpressure
        // 2MB buffer prevents OOM while maintaining good throughput
        let (reader, writer) = tokio::io::simplex(2 * 1024 * 1024);

        // Spawn async task that writes to the buffered writer
        tokio::task::spawn(async move {
            if let Err(e) = Self::create_streaming_archive_async(
                &config,
                &supergraph_schema,
                &router_config,
                writer,
            )
            .await
            {
                tracing::error!("Failed to create streaming archive: {}", e);
            }
        });

        // Convert reader to stream of Result<Bytes, BoxError>
        ReaderStream::new(reader).map(|result| result.map_err(|e| Box::new(e) as BoxError))
    }

    /// Creates the streaming archive asynchronously using tokio::io for streaming
    ///
    /// This function accepts any `AsyncWrite` implementation (typically the write half
    /// of a `SimplexStream`) and writes a complete diagnostic archive containing:
    /// - Main manifest with system information and file descriptions
    /// - Router configuration as YAML
    /// - Supergraph schema as GraphQL
    /// - Router binary (or placeholder in test mode)
    /// - Memory diagnostic files (platform-dependent)
    /// - System information report
    ///
    /// All data is written directly to the writer with streaming I/O, ensuring
    /// memory usage remains bounded regardless of archive size.
    async fn create_streaming_archive_async<
        W: tokio::io::AsyncWrite + Unpin + Send + Sync + 'static,
    >(
        config: &Config,
        supergraph_schema: &str,
        router_config: &str,
        writer: W,
    ) -> DiagnosticsResult<()> {
        // Use tokio AsyncWrite directly with tokio ecosystem
        // This provides built-in backpressure within tokio
        let encoder = GzipEncoder::new(writer);
        let mut tar = tokio_tar::Builder::new(encoder);

        // Add files to archive - each write operation streams chunks to the client
        // Files are processed in order of increasing size to start the download quickly:
        // 1. Small metadata files first (manifest, config, schema, system info)
        // 2. Potentially large files last (memory dumps)
        Self::add_manifest_to_archive_async(&mut tar, config).await?;
        Self::add_router_config_to_archive_async(&mut tar, router_config).await?;
        Self::add_supergraph_schema_to_archive_async(&mut tar, supergraph_schema).await?;
        Self::add_system_info_to_archive_async(&mut tar).await?;
        Self::add_memory_data_to_archive_async(&mut tar, &config.output_directory).await?;
        Self::add_html_report_to_archive_async(&mut tar, config, router_config, supergraph_schema)
            .await?;
        // Router binary no longer needed - self-contained HTML report replaces analyze.sh

        // Finalize the archive and ensure all buffered data is flushed
        // The async tar builder and gzip encoder stream data incrementally
        let mut encoder = tar
            .into_inner()
            .await
            .map_err(|e| DiagnosticsError::Internal(format!("Failed to finalize tar: {}", e)))?;
        encoder
            .shutdown()
            .await
            .map_err(|e| DiagnosticsError::Internal(format!("Failed to finalize gzip: {}", e)))?;

        // At this point, all archive data has been streamed to the client
        // without accumulating large files in memory
        Ok(())
    }

    /// Add the manifest file to the archive with async I/O
    async fn add_manifest_to_archive_async<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        config: &Config,
    ) -> DiagnosticsResult<()> {
        let manifest = Self::create_main_manifest(config)?;
        ArchiveUtils::add_text_file(tar, "manifest.txt", &manifest).await
    }

    /// Add the router configuration to the archive with async I/O
    async fn add_router_config_to_archive_async<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        router_config: &str,
    ) -> DiagnosticsResult<()> {
        ArchiveUtils::add_text_file(tar, "router.yaml", router_config).await
    }

    /// Add the supergraph schema to the archive with async I/O
    async fn add_supergraph_schema_to_archive_async<
        W: tokio::io::AsyncWrite + Unpin + Send + Sync,
    >(
        tar: &mut tokio_tar::Builder<W>,
        supergraph_schema: &str,
    ) -> DiagnosticsResult<()> {
        ArchiveUtils::add_text_file(tar, "supergraph.graphql", supergraph_schema).await
    }

    /// Add system information to the archive with async I/O
    async fn add_system_info_to_archive_async<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
    ) -> DiagnosticsResult<()> {
        let system_info = crate::plugins::diagnostics::system_info::collect().await?;
        ArchiveUtils::add_text_file(tar, "system_info.txt", &system_info).await
    }

    /// Add memory profiling data to the archive with async I/O
    async fn add_memory_data_to_archive_async<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        output_directory: &str,
    ) -> DiagnosticsResult<()> {
        // The memory module now handles platform differences internally
        memory::MemoryService::add_to_archive(tar, output_directory).await
    }

    /// Add the HTML diagnostic report to the archive
    async fn add_html_report_to_archive_async<W: tokio::io::AsyncWrite + Unpin + Send + Sync>(
        tar: &mut tokio_tar::Builder<W>,
        config: &Config,
        router_config: &str,
        supergraph_schema: &str,
    ) -> DiagnosticsResult<()> {
        use std::path::Path;

        use crate::plugins::diagnostics::html_generator::HtmlGenerator;
        use crate::plugins::diagnostics::html_generator::ReportData;

        // Create HTML generator
        let generator = HtmlGenerator::new()?;

        // Get system info content
        let system_info_content = crate::plugins::diagnostics::system_info::collect().await?;

        // Read memory dumps from the directory using the memory module
        let memory_directory = Path::new(&config.output_directory).join("memory");
        let memory_dumps = memory::load_memory_dumps(&memory_directory).await?;

        // Generate the HTML report with all embedded data
        let report_data = ReportData::new(
            Some(&system_info_content),
            Some(router_config),
            Some(supergraph_schema),
            &memory_dumps,
        );
        let html_content = generator.generate_embedded_html(report_data)?;

        // Add HTML report to archive using archive utilities
        ArchiveUtils::add_text_file(tar, "diagnostics_report.html", &html_content).await
    }

    /// Creates the main manifest for the diagnostic archive
    fn create_main_manifest(config: &Config) -> DiagnosticsResult<String> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to get current timestamp: {}", e))
            })?
            .as_secs();

        let memory_profiling_info = if cfg!(target_family = "unix") {
            "Memory Profiling: Enabled (jemalloc profiling available)"
        } else {
            "Memory Profiling: Not available - Heap dumps require Linux platform with jemalloc"
        };

        let manifest = format!(
            "APOLLO ROUTER DIAGNOSTIC ARCHIVE\n\
            Generated: {}\n\
            Router Version: {}\n\
            Platform: {}\n\
            Memory Output Directory: {}\n\
            {}\n\
            \n\
            Contents: manifest.txt, router.yaml, supergraph.graphql, system_info.txt, memory/, diagnostics_report.html\n",
            timestamp,
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            config.output_directory,
            memory_profiling_info
        );

        Ok(manifest)
    }

    /// Get the configuration for testing purposes
    #[cfg(test)]
    pub(super) fn config(&self) -> &Config {
        &self.config
    }
}
