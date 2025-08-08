//! Service implementation for diagnostics plugin with internal routing
//!
//! **Platform Support**: This service is only available on Linux platforms.

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use base64::Engine as _;
use base64::engine::general_purpose;
use http::Method;
use http::StatusCode;
use serde_json::json;
use tower::BoxError;
use tower::Service;

use super::memory::MemoryService;
use crate::services::router::Request;
use crate::services::router::Response;
use crate::services::router::body;

/// Internal service that handles diagnostics requests with authentication and routing
#[derive(Clone)]
pub(super) struct DiagnosticsService {
    shared_secret: String,
    memory: MemoryService,
    diagnostics_plugin: std::sync::Arc<super::Diagnostics>,
}

impl DiagnosticsService {
    pub(super) fn new(shared_secret: String, output_directory: String, diagnostics_plugin: std::sync::Arc<super::Diagnostics>) -> Self {
        Self {
            shared_secret,
            memory: MemoryService::new(output_directory),
            diagnostics_plugin,
        }
    }

    /// Authenticate request using Bearer token (base64 encoded)
    fn authenticate(&self, request: &Request) -> Result<(), StatusCode> {
        let auth_header = request
            .router_request
            .headers()
            .get("authorization")
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let auth_str = auth_header.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;

        if auth_str.starts_with("Bearer ") {
            let encoded_token = &auth_str[7..]; // Remove "Bearer " prefix
            
            // Decode the base64 token
            if let Ok(decoded_bytes) = general_purpose::STANDARD.decode(encoded_token) {
                if let Ok(decoded_token) = String::from_utf8(decoded_bytes) {
                    if decoded_token == self.shared_secret {
                        return Ok(());
                    }
                }
            }
        }

        Err(StatusCode::UNAUTHORIZED)
    }

    /// Route request to appropriate handler based on path
    async fn route_request(&self, request: Request) -> Result<Response, BoxError> {
        // Extract the path from the URI, removing the /diagnostics prefix
        let full_path = request.router_request.uri().path();
        let path = if let Some(stripped) = full_path.strip_prefix("/diagnostics/") {
            stripped.to_string()
        } else if full_path == "/diagnostics" {
            String::new()
        } else {
            full_path.to_string()
        };
        let method = request.router_request.method().clone();

        tracing::info!(path = %path, full_path = %full_path, method = %method, "Routing diagnostics request");

        let result = match (&method, path.as_str()) {
            (&Method::GET, "memory/status") => {
                tracing::info!("Routing to memory.handle_status()");
                self.memory.handle_status(request).await
            }
            (&Method::POST, "memory/start") => {
                tracing::info!("Routing to memory.handle_start()");
                self.memory.handle_start(request).await
            }
            (&Method::POST, "memory/stop") => {
                tracing::info!("Routing to memory.handle_stop()");
                self.memory.handle_stop(request).await
            }
            (&Method::POST, "memory/dump") => {
                tracing::info!("Routing to memory.handle_dump()");
                self.memory.handle_dump(request).await
            }
            (&Method::GET, "export") => {
                tracing::info!("Routing to diagnostics.handle_export()");
                self.diagnostics_plugin.handle_export(request).await
            }
            _ => {
                tracing::warn!("No route found for {} {}, returning 404", method, path);
                self.error_response(StatusCode::NOT_FOUND, request)
            }
        };
        
        tracing::info!("Request handling completed");
        result
    }

    /// Create error response
    fn error_response(&self, status: StatusCode, request: Request) -> Result<Response, BoxError> {
        let message = match status {
            StatusCode::UNAUTHORIZED => {
                "Authentication required. Use 'Authorization: Bearer <base64(secret)>' header."
            }
            StatusCode::NOT_FOUND => {
                "Endpoint not found. Available: GET export, GET memory/status, POST memory/start, POST memory/stop, POST memory/dump"
            }
            _ => "Internal server error",
        };

        let error = json!({
            "error": message,
            "status": status.as_u16()
        });

        Ok(Response::http_response_builder()
            .response(
                http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(body::from_bytes(serde_json::to_vec(&error)?))?,
            )
            .context(request.context)
            .build()?)
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
        Box::pin(async move {
            let uri = req.router_request.uri();
            let method = req.router_request.method();
            tracing::info!("DiagnosticsService received request: {} {}", method, uri);
            
            // Authenticate first
            if let Err(status) = service.authenticate(&req) {
                tracing::warn!("Authentication failed for request: {} {}", method, uri);
                return service.error_response(status, req);
            }

            tracing::info!("Authentication successful for request: {} {}", method, uri);
            
            // Route request
            service.route_request(req).await
        })
    }
}
