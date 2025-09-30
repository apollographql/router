//! JavaScript resource handler for diagnostics plugin
//!
//! This module provides utilities for serving embedded JavaScript resources
//! used by the diagnostics HTML interface. All JavaScript files are embedded
//! at compile time.

use super::constants::routes::js_resources;

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

    /// Get JavaScript resource content by path
    pub(super) fn get_resource(&self, path: &str) -> Option<&'static str> {
        self.resources
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.content)
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
    use super::*;

    #[test]
    fn test_js_resource_handler_creation() {
        let handler = JsResourceHandler::new();
        assert_eq!(handler.resources.len(), 7);
    }

    #[test]
    fn test_get_valid_resource() {
        let handler = JsResourceHandler::new();

        let result = handler.get_resource(js_resources::BACKTRACE_PROCESSOR);

        assert!(result.is_some());
        let content = result.unwrap();
        assert!(!content.is_empty(), "Resource content should not be empty");
    }

    #[test]
    fn test_get_invalid_resource() {
        let handler = JsResourceHandler::new();

        let result = handler.get_resource("/unknown.js");
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
