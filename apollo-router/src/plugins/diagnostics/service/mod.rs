//! HTTP service implementation for diagnostics plugin
//!
//! Implements the Axum router and HTTP handlers for all diagnostic endpoints.
//! The service provides a REST-like API with the following structure:
//!
//! ## Endpoints
//!
//! **Dashboard & Configuration:**
//! - `GET /` - Interactive HTML dashboard
//! - `GET /system_info.txt` - System diagnostic information
//! - `GET /router_config.yaml` - Active router configuration
//! - `GET /supergraph.graphql` - Supergraph schema
//! - `GET /export` - Complete diagnostic archive (.tar.gz)
//!
//! **Memory Profiling:**
//! - `GET /memory/status` - Current profiling state (active/inactive)
//! - `POST /memory/start` - Begin heap profiling
//! - `POST /memory/stop` - End heap profiling
//! - `POST /memory/dump` - Create instant heap snapshot
//! - `GET /memory/dumps` - List all heap dump files
//! - `DELETE /memory/dumps` - Remove all heap dumps
//! - `GET /memory/dumps/:filename` - Download specific dump
//! - `DELETE /memory/dumps/:filename` - Delete specific dump
//!
//! **JavaScript Resources:**
//! - Fallback handler serves embedded JS files for dashboard visualization
//!
//! All responses include appropriate cache-control headers to prevent stale diagnostics data.
//!
//! ## Platform Support
//!
//! Available on all platforms. Memory profiling endpoints return "not supported" responses
//! on non-Linux platforms or when jemalloc is not enabled.

use std::sync::Arc;

use axum::Extension;
use axum::Router;
use axum::extract::Path;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use http::StatusCode;
use mime::TEXT_HTML_UTF_8;
use mime::TEXT_PLAIN_UTF_8;

use super::constants;
use super::export::Exporter;
use super::html_generator::HtmlGenerator;
use super::static_resources::StaticResourceHandler;
use super::memory::MemoryService;

#[cfg(test)]
mod tests;

/// MIME type for YAML content
const TEXT_YAML: &str = "text/yaml; charset=utf-8";

/// Shared state for all diagnostics handlers
#[derive(Clone)]
struct DiagnosticsState {
    memory: MemoryService,
    static_resources: StaticResourceHandler,
    router_config: Arc<str>,
    supergraph_schema: Arc<String>,
    output_directory: String,
}

/// Creates the diagnostics router with all routes configured
pub(super) fn create_router(
    output_directory: String,
    router_config: Arc<str>,
    supergraph_schema: Arc<String>,
) -> Router {
    let state = DiagnosticsState {
        memory: MemoryService::new(output_directory.clone()),
        static_resources: StaticResourceHandler::new(),
        router_config,
        supergraph_schema,
        output_directory,
    };

    Router::new()
        // Dashboard
        .route("/", get(handle_dashboard))
        // System information and configuration
        .route("/system_info.txt", get(handle_system_info))
        .route("/router_config.yaml", get(handle_router_config))
        .route("/supergraph.graphql", get(handle_supergraph_schema))
        // Export
        .route("/export", get(handle_export))
        // Memory profiling endpoints
        .route("/memory/status", get(handle_memory_status))
        .route(
            "/memory/dumps",
            get(handle_memory_list_dumps).delete(handle_memory_clear_dumps),
        )
        .route("/memory/start", post(handle_memory_start))
        .route("/memory/stop", post(handle_memory_stop))
        .route("/memory/dump", post(handle_memory_dump))
        .route(
            "/memory/dumps/{filename}",
            get(handle_memory_download_dump).delete(handle_memory_delete_dump),
        )
        // Static resources (JS/CSS) - fallback for any unmatched routes
        .fallback(handle_fallback)
        .layer(Extension(state))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Helper to convert Result into Response with error handling
fn result_to_response<T, E>(
    result: Result<T, E>,
    error_message: &str,
) -> Response
where
    T: IntoResponse,
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {}", error_message, e),
        )
            .into_response(),
    }
}

/// Helper for 404 Not Found errors
fn not_found_response(message: impl std::fmt::Display) -> Response {
    (StatusCode::NOT_FOUND, format!("{}", message)).into_response()
}

// ============================================================================
// Handler Functions
// ============================================================================

/// GET / - Serve the interactive dashboard
async fn handle_dashboard() -> Response {
    let result = HtmlGenerator::new().and_then(|g| g.generate_dashboard_html());
    match result {
        Ok(html) => (
            StatusCode::OK,
            [(http::header::CONTENT_TYPE, TEXT_HTML_UTF_8.as_ref())],
            html,
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to generate dashboard",
        )
            .into_response(),
    }
}

/// GET /system_info.txt - Collect and return system information
async fn handle_system_info() -> Response {
    result_to_response(
        super::system_info::collect().await.map(|info| {
            (
                StatusCode::OK,
                [(http::header::CONTENT_TYPE, TEXT_PLAIN_UTF_8.as_ref())],
                info,
            )
        }),
        "Failed to collect system info",
    )
}

/// GET /router_config.yaml - Return router configuration
async fn handle_router_config(Extension(state): Extension<DiagnosticsState>) -> Response {
    (
        StatusCode::OK,
        [(http::header::CONTENT_TYPE, TEXT_YAML)],
        state.router_config.to_string(),
    )
        .into_response()
}

/// GET /supergraph.graphql - Return supergraph schema
async fn handle_supergraph_schema(Extension(state): Extension<DiagnosticsState>) -> Response {
    (
        StatusCode::OK,
        [(http::header::CONTENT_TYPE, TEXT_PLAIN_UTF_8.as_ref())],
        state.supergraph_schema.to_string(),
    )
        .into_response()
}

/// GET /export - Export diagnostics data
async fn handle_export(Extension(state): Extension<DiagnosticsState>) -> Response {
    // Create exporter on-demand since export() consumes self
    let exporter = Exporter::new(
        super::Config {
            enabled: true,
            listen: constants::network::default_listen_addr().into(),
            output_directory: state.output_directory.clone(),
        },
        state.supergraph_schema.clone(),
        state.router_config.clone(),
    );

    result_to_response(exporter.export().await, "Export failed")
}

/// GET /memory/status - Get memory profiling status
async fn handle_memory_status(Extension(state): Extension<DiagnosticsState>) -> Response {
    result_to_response(state.memory.handle_status().await, "Failed to get memory status")
}

/// GET /memory/dumps - List all memory dumps
async fn handle_memory_list_dumps(Extension(state): Extension<DiagnosticsState>) -> Response {
    result_to_response(state.memory.handle_list_dumps().await, "Failed to list dumps")
}

/// DELETE /memory/dumps - Clear all memory dumps
async fn handle_memory_clear_dumps(Extension(state): Extension<DiagnosticsState>) -> Response {
    result_to_response(state.memory.handle_clear_all_dumps().await, "Failed to clear dumps")
}

/// POST /memory/start - Start memory profiling
async fn handle_memory_start(Extension(state): Extension<DiagnosticsState>) -> Response {
    result_to_response(state.memory.handle_start().await, "Failed to start profiling")
}

/// POST /memory/stop - Stop memory profiling
async fn handle_memory_stop(Extension(state): Extension<DiagnosticsState>) -> Response {
    result_to_response(state.memory.handle_stop().await, "Failed to stop profiling")
}

/// POST /memory/dump - Create a memory dump
async fn handle_memory_dump(Extension(state): Extension<DiagnosticsState>) -> Response {
    result_to_response(state.memory.handle_dump().await, "Failed to create dump")
}

/// GET /memory/dumps/:filename - Download a specific memory dump
async fn handle_memory_download_dump(
    Extension(state): Extension<DiagnosticsState>,
    Path(filename): Path<String>,
) -> Response {
    state
        .memory
        .handle_download_dump(&filename)
        .await
        .map_or_else(|e| not_found_response(format!("Dump not found: {}", e)), |r| r.into_response())
}

/// DELETE /memory/dumps/:filename - Delete a specific memory dump
async fn handle_memory_delete_dump(
    Extension(state): Extension<DiagnosticsState>,
    Path(filename): Path<String>,
) -> Response {
    state
        .memory
        .handle_delete_dump(&filename)
        .await
        .map_or_else(|e| not_found_response(format!("Dump not found: {}", e)), |r| r.into_response())
}

/// Fallback handler for unmatched routes (static resources: JS/CSS)
async fn handle_fallback(
    Extension(state): Extension<DiagnosticsState>,
    uri: http::Uri,
) -> Response {
    // Since we're nested under /diagnostics, axum has already stripped that prefix
    // We just need to remove the leading slash to match our resource paths
    let path = uri.path().strip_prefix('/').unwrap_or(uri.path());

    match state.static_resources.get_resource(path) {
        Some((content, content_type)) => (
            StatusCode::OK,
            [(http::header::CONTENT_TYPE, content_type)],
            content,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}
