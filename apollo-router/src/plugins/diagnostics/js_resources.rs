//! JavaScript resource handler for diagnostics plugin
//!
//! This module provides utilities for serving embedded JavaScript resources
//! used by the diagnostics HTML interface. All JavaScript files are embedded
//! at compile time and served with proper caching headers.

use http::StatusCode;
use mime::TEXT_JAVASCRIPT;

use super::DiagnosticsResult;
use super::constants::routes::js_resources;
use super::response_builder::CacheControl;
use super::response_builder::ResponseBuilder;
use crate::Context;
use crate::services::router::Response;

/// JavaScript resource definitions with their paths and content
struct JsResource {
    /// Route path for this resource
    path: &'static str,
    /// Embedded JavaScript content
    content: &'static str,
}

/// Registry of all available JavaScript resources
#[derive(Clone)]
pub(super) struct JsResourceHandler {
    resources: &'static [JsResource],
}

impl JsResourceHandler {
    /// Create a new JavaScript resource handler with all embedded resources
    pub(super) fn new() -> Self {
        static RESOURCES: &[JsResource] = &[
            JsResource {
                path: js_resources::BACKTRACE_PROCESSOR,
                content: include_str!("resources/backtrace-processor.js"),
            },
            JsResource {
                path: js_resources::VIZ_JS_INTEGRATION,
                content: include_str!("resources/viz-js-integration.js"),
            },
            JsResource {
                path: js_resources::FLAMEGRAPH_RENDERER,
                content: include_str!("resources/flamegraph-renderer.js"),
            },
            JsResource {
                path: js_resources::CALLGRAPH_SVG_RENDERER,
                content: include_str!("resources/callgraph-svg-renderer.js"),
            },
            JsResource {
                path: js_resources::DATA_ACCESS,
                content: include_str!("resources/data-access.js"),
            },
            JsResource {
                path: js_resources::MAIN,
                content: include_str!("resources/main.js"),
            },
            JsResource {
                path: js_resources::CUSTOM_ELEMENTS,
                content: include_str!("resources/custom_elements.js"),
            },
        ];

        Self {
            resources: RESOURCES,
        }
    }

    /// Handle a JavaScript resource request by path
    pub(super) fn handle_request(
        &self,
        path: &str,
        context: Context,
    ) -> Option<DiagnosticsResult<Response>> {
        // Find the resource by path
        let resource = self.resources.iter().find(|r| r.path == path)?;

        // Serve the JavaScript content with static resource caching
        Some(ResponseBuilder::text_response(
            StatusCode::OK,
            TEXT_JAVASCRIPT,
            resource.content,
            CacheControl::StaticResource,
            context,
        ))
    }
    /// Get all JavaScript resource paths
    #[cfg(test)]
    pub(super) fn get_all_paths(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(|r| r.path)
    }
}

impl Default for JsResourceHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::*;
    use crate::services::router::Request;
    use crate::services::router::{self};

    fn create_test_request() -> Request {
        router::Request::fake_builder()
            .method(Method::GET)
            .uri(http::Uri::from_static("http://localhost/test"))
            .build()
            .unwrap()
    }

    #[test]
    fn test_js_resource_handler_creation() {
        let handler = JsResourceHandler::new();
        assert_eq!(handler.resources.len(), 7);
    }

    #[tokio::test]
    async fn test_handle_valid_resource() {
        let handler = JsResourceHandler::new();
        let request = create_test_request();

        let result =
            handler.handle_request(js_resources::BACKTRACE_PROCESSOR, request.context.clone());

        assert!(result.is_some());
        let response = result.unwrap().unwrap();
        assert_eq!(response.response.status(), StatusCode::OK);

        // Verify content-type header
        assert_eq!(
            response
                .response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .unwrap(),
            "text/javascript"
        );
    }

    #[tokio::test]
    async fn test_handle_invalid_resource() {
        let handler = JsResourceHandler::new();
        let request = create_test_request();

        let result = handler.handle_request("/unknown.js", request.context.clone());
        assert!(result.is_none());
    }

    #[test]
    fn test_all_resource_paths_defined() {
        let handler = JsResourceHandler::new();
        let paths: Vec<_> = handler.get_all_paths().collect();

        // Verify all expected paths are present
        assert!(paths.contains(&js_resources::BACKTRACE_PROCESSOR));
        assert!(paths.contains(&js_resources::VIZ_JS_INTEGRATION));
        assert!(paths.contains(&js_resources::FLAMEGRAPH_RENDERER));
        assert!(paths.contains(&js_resources::CALLGRAPH_SVG_RENDERER));
        assert!(paths.contains(&js_resources::DATA_ACCESS));
        assert!(paths.contains(&js_resources::MAIN));
        assert!(paths.contains(&js_resources::CUSTOM_ELEMENTS));
    }
}
