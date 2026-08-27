//! Custom web endpoints that plugins register through [`Plugin::web_endpoints`](crate::plugin::Plugin::web_endpoints).

use axum::response::IntoResponse;
use http::StatusCode;
use tower::ServiceExt;
use tower::service_fn;

use crate::plugin::Handler;
use crate::services::router;

#[derive(Clone)]
/// A path and a handler to be exposed as a web_endpoint for plugins
pub struct Endpoint {
    pub(crate) path: String,
    // Plugins need to be Send + Sync
    // BoxCloneService isn't enough
    handler: EndpointHandler,
}

#[derive(Clone)]
enum EndpointHandler {
    /// Legacy handler wrapping a router service
    Service(Handler),
    /// Direct axum router (bypasses service conversion)
    Router(axum::Router),
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("path", &self.path)
            .finish()
    }
}

impl Endpoint {
    /// Creates an Endpoint given a path and a Boxed Service
    pub fn from_router_service(path: String, handler: router::BoxCloneService) -> Self {
        Self {
            path,
            handler: EndpointHandler::Service(Handler::new(handler)),
        }
    }

    /// Creates an Endpoint given a path and an axum Router
    ///
    /// This is the preferred method for plugins that use axum internally,
    /// as it avoids unnecessary service wrapping and path manipulation.
    ///
    /// The router will be automatically nested at the specified path, allowing
    /// it to handle all sub-routes. For example, a router registered at `/diagnostics`
    /// will handle `/diagnostics/`, `/diagnostics/memory/status`, etc.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axum::{Router, routing::get};
    ///
    /// let router = Router::new()
    ///     .route("/", get(handle_dashboard))
    ///     .route("/status", get(handle_status));
    ///
    /// let endpoint = Endpoint::from_router("/diagnostics".to_string(), router);
    /// // This will handle:
    /// // - /diagnostics/
    /// // - /diagnostics/status
    /// ```
    pub(crate) fn from_router(path: String, router: axum::Router) -> Self {
        Self {
            path,
            handler: EndpointHandler::Router(router),
        }
    }

    pub(crate) fn into_router(self) -> axum::Router {
        match self.handler {
            // If we already have a router, just nest it at the path
            EndpointHandler::Router(router) => axum::Router::new().nest(&self.path, router),
            // Legacy service handling with path-based routing
            EndpointHandler::Service(handler) => {
                let handler_clone = handler.clone();
                let handler = move |req: http::Request<axum::body::Body>| {
                    let endpoint = handler_clone.clone();
                    async move {
                        Ok(endpoint
                            .oneshot(req.into())
                            .await
                            .map(|res| res.response)
                            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
                            .into_response())
                    }
                };

                axum::Router::new().route_service(self.path.as_str(), service_fn(handler))
            }
        }
    }
}
