//! Service implementation for diagnostics plugin with internal routing
//!
//! **Platform Support**: This service is available on all platforms.
//! Memory profiling features are available with graceful degradation on non-Linux platforms.

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::task::Context;
use std::task::Poll;

use http::Method;
use http::StatusCode;
use mime::Mime;
use mime::TEXT_HTML_UTF_8;
use mime::TEXT_PLAIN_UTF_8;
use tower::BoxError;
use tower::Service;

use super::DiagnosticsResult;
use super::constants;
use super::export::Exporter;
use super::html_generator::HtmlGenerator;
use super::js_resources::JsResourceHandler;
use super::memory::MemoryService;
use super::response_builder::CacheControl;
use super::response_builder::ResponseBuilder;
use crate::services::router::Request;
use crate::services::router::Response;

/// Internal service that handles diagnostics requests with routing
#[derive(Clone)]
pub(super) struct DiagnosticsService {
    memory: MemoryService,
    exporter: Exporter,
    js_resources: JsResourceHandler,
    router_config: std::sync::Arc<String>,
    supergraph_schema: std::sync::Arc<String>,
}

impl DiagnosticsService {
    pub(super) fn new(
        output_directory: String,
        exporter: Exporter,
        router_config: std::sync::Arc<String>,
        supergraph_schema: std::sync::Arc<String>,
    ) -> Self {
        Self {
            memory: MemoryService::new(output_directory),
            exporter,
            js_resources: JsResourceHandler::new(),
            router_config,
            supergraph_schema,
        }
    }

    /// Route request to appropriate handler based on path
    async fn route_request(&self, request: Request) -> DiagnosticsResult<Response> {
        let full_path = request.router_request.uri().path();

        // SECURITY: Safe path prefix stripping
        // We strip the "/diagnostics/" prefix to get the internal route
        // This is safe because we only match known routes below
        let path = if full_path == constants::routes::BASE {
            ""
        } else {
            full_path
                .strip_prefix(&format!("{}/", constants::routes::BASE))
                .unwrap_or(full_path)
        };
        let method = request.router_request.method();

        // SECURITY: Extract filename for dump operations with controlled path parsing
        // We use strip_prefix to safely extract just the filename portion
        // Further validation happens in the dump handlers to prevent path traversal
        let dump_filename = if path.starts_with(constants::routes::memory::DUMPS_PREFIX) {
            path.strip_prefix(constants::routes::memory::DUMPS_PREFIX)
                .map(|s| s.to_string())
        } else {
            None
        };

        // Check for JavaScript resources first before other routes
        if method == Method::GET
            && let Some(resource) = self
                .js_resources
                .handle_request(path, request.context.clone())
        {
            return resource;
        }

        match (method, path) {
            (&Method::GET, "") => self.handle_dashboard(request).await,
            (&Method::GET, constants::routes::memory::STATUS) => {
                self.memory.handle_status(request).await
            }
            (&Method::GET, constants::routes::memory::DUMPS) => {
                self.memory.handle_list_dumps(request).await
            }
            (&Method::DELETE, constants::routes::memory::DUMPS) => {
                self.memory.handle_clear_all_dumps(request).await
            }
            (&Method::POST, constants::routes::memory::START) => {
                self.memory.handle_start(request).await
            }
            (&Method::POST, constants::routes::memory::STOP) => {
                self.memory.handle_stop(request).await
            }
            (&Method::POST, constants::routes::memory::DUMP) => {
                self.memory.handle_dump(request).await
            }
            (&Method::GET, constants::routes::EXPORT) => {
                self.exporter.clone().export(request).await
            }
            (&Method::GET, constants::routes::SYSTEM_INFO) => {
                self.handle_system_info(request).await
            }
            (&Method::GET, constants::routes::ROUTER_CONFIG) => {
                self.handle_router_config(request).await
            }
            (&Method::GET, constants::routes::SUPERGRAPH_SCHEMA) => {
                self.handle_supergraph_schema(request).await
            }
            (&Method::GET, dump_path)
                if dump_path.starts_with(constants::routes::memory::DUMPS_PREFIX) =>
            {
                self.handle_memory_dump_operation(request, dump_filename, false)
                    .await
            }
            (&Method::DELETE, dump_path)
                if dump_path.starts_with(constants::routes::memory::DUMPS_PREFIX) =>
            {
                self.handle_memory_dump_operation(request, dump_filename, true)
                    .await
            }
            _ => self.error_response(StatusCode::NOT_FOUND, request),
        }
    }

    /// Handle GET /diagnostics - serve interactive dashboard
    async fn handle_dashboard(&self, request: Request) -> DiagnosticsResult<Response> {
        // Create HTML generator on-demand for dashboard
        let html_generator = HtmlGenerator::new()?;

        // Generate dashboard HTML with separate script tags (not inlined)
        let html = html_generator.generate_dashboard_html()?;

        ResponseBuilder::text_response(
            StatusCode::OK,
            TEXT_HTML_UTF_8,
            html,
            CacheControl::NoCache,
            request.context.clone(),
        )
    }

    /// Handle GET /diagnostics/system_info.txt
    async fn handle_system_info(&self, request: Request) -> DiagnosticsResult<Response> {
        // Collect system information using the system_info module
        let system_info = super::system_info::collect().await?;

        ResponseBuilder::text_response(
            StatusCode::OK,
            TEXT_PLAIN_UTF_8,
            system_info,
            CacheControl::NoCache,
            request.context.clone(),
        )
    }

    /// Handle GET /diagnostics/router_config.yaml
    async fn handle_router_config(&self, request: Request) -> DiagnosticsResult<Response> {
        // Return the actual router configuration
        let config_yaml = self.router_config.as_str().to_string();

        ResponseBuilder::text_response(
            StatusCode::OK,
            Mime::from_str("text/yaml").expect("valid mime type"),
            config_yaml,
            CacheControl::NoCache,
            request.context.clone(),
        )
    }

    /// Handle GET /diagnostics/supergraph.graphql
    async fn handle_supergraph_schema(&self, request: Request) -> DiagnosticsResult<Response> {
        // Return the actual supergraph schema
        let schema = self.supergraph_schema.as_str().to_string();

        ResponseBuilder::text_response(
            StatusCode::OK,
            TEXT_PLAIN_UTF_8,
            schema,
            CacheControl::NoCache,
            request.context.clone(),
        )
    }

    /// Handle memory dump operations (GET or DELETE) with unified filename validation
    async fn handle_memory_dump_operation(
        &self,
        request: Request,
        dump_filename: Option<String>,
        is_delete: bool,
    ) -> DiagnosticsResult<Response> {
        if let Some(filename) = dump_filename {
            if is_delete {
                self.memory.handle_delete_dump(request, &filename).await
            } else {
                self.memory.handle_download_dump(request, &filename).await
            }
        } else {
            self.error_response(StatusCode::NOT_FOUND, request)
        }
    }

    /// Create error response
    fn error_response(&self, status: StatusCode, request: Request) -> DiagnosticsResult<Response> {
        let message = match status {
            StatusCode::NOT_FOUND => constants::messages::errors::NOT_FOUND,
            _ => constants::messages::errors::INTERNAL_ERROR,
        };

        ResponseBuilder::error_response(status, message, request.context.clone())
    }
}

impl Service<Request> for DiagnosticsService {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { service.route_request(req).await.map_err(|e| e.into()) })
    }
}
