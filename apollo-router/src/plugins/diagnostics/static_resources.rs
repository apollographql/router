//! Static resource handler for diagnostics plugin
//!
//! This module provides utilities for serving embedded static resources
//! (JavaScript, CSS, etc.) used by the diagnostics HTML interface.
//! All resources are embedded at compile time for zero-dependency deployment.

use super::constants::routes;

/// Static resource definitions with their paths, content, and MIME types
struct StaticResource {
    /// Route path for this resource
    path: &'static str,
    /// Embedded resource content
    content: &'static str,
    /// MIME content type for HTTP responses
    content_type: &'static str,
}

/// Registry of all available static resources
#[derive(Clone)]
pub(super) struct StaticResourceHandler {
    resources: &'static [StaticResource],
}

impl StaticResourceHandler {
    /// Create a new static resource handler with all embedded resources
    pub(super) fn new() -> Self {
        static RESOURCES: &[StaticResource] = &[
            // JavaScript resources
            StaticResource {
                path: routes::js_resources::BACKTRACE_PROCESSOR,
                content: include_str!("resources/backtrace-processor.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            StaticResource {
                path: routes::js_resources::VIZ_JS_INTEGRATION,
                content: include_str!("resources/viz-js-integration.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            StaticResource {
                path: routes::js_resources::FLAMEGRAPH_RENDERER,
                content: include_str!("resources/flamegraph-renderer.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            StaticResource {
                path: routes::js_resources::CALLGRAPH_SVG_RENDERER,
                content: include_str!("resources/callgraph-svg-renderer.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            StaticResource {
                path: routes::js_resources::DATA_ACCESS,
                content: include_str!("resources/data-access.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            StaticResource {
                path: routes::js_resources::MAIN,
                content: include_str!("resources/main.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            StaticResource {
                path: routes::js_resources::CUSTOM_ELEMENTS,
                content: include_str!("resources/custom_elements.js"),
                content_type: "application/javascript; charset=utf-8",
            },
            // CSS resources
            StaticResource {
                path: routes::css_resources::STYLES,
                content: include_str!("resources/styles.css"),
                content_type: "text/css; charset=utf-8",
            },
        ];

        Self {
            resources: RESOURCES,
        }
    }

    /// Get static resource content and content type by path
    pub(super) fn get_resource(&self, path: &str) -> Option<(&'static str, &'static str)> {
        self.resources
            .iter()
            .find(|r| r.path == path)
            .map(|r| (r.content, r.content_type))
    }

    /// Get all static resource paths
    #[cfg(test)]
    pub(super) fn get_all_paths(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(|r| r.path)
    }
}

impl Default for StaticResourceHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_resource_handler_creation() {
        let handler = StaticResourceHandler::new();
        assert_eq!(handler.resources.len(), 8); // 7 JS + 1 CSS
    }

    #[test]
    fn test_get_valid_js_resource() {
        let handler = StaticResourceHandler::new();

        let result = handler.get_resource(routes::js_resources::BACKTRACE_PROCESSOR);

        assert!(result.is_some());
        let (content, content_type) = result.unwrap();
        assert!(!content.is_empty(), "Resource content should not be empty");
        assert_eq!(content_type, "application/javascript; charset=utf-8");
    }

    #[test]
    fn test_get_valid_css_resource() {
        let handler = StaticResourceHandler::new();

        let result = handler.get_resource(routes::css_resources::STYLES);

        assert!(result.is_some());
        let (content, content_type) = result.unwrap();
        assert!(!content.is_empty(), "CSS content should not be empty");
        assert_eq!(content_type, "text/css; charset=utf-8");
    }

    #[test]
    fn test_get_invalid_resource() {
        let handler = StaticResourceHandler::new();

        let result = handler.get_resource("/unknown.js");
        assert!(result.is_none());
    }

    #[test]
    fn test_all_resource_paths_defined() {
        let handler = StaticResourceHandler::new();
        let paths: Vec<_> = handler.get_all_paths().collect();

        // Verify all expected JS paths are present
        assert!(paths.contains(&routes::js_resources::BACKTRACE_PROCESSOR));
        assert!(paths.contains(&routes::js_resources::VIZ_JS_INTEGRATION));
        assert!(paths.contains(&routes::js_resources::FLAMEGRAPH_RENDERER));
        assert!(paths.contains(&routes::js_resources::CALLGRAPH_SVG_RENDERER));
        assert!(paths.contains(&routes::js_resources::DATA_ACCESS));
        assert!(paths.contains(&routes::js_resources::MAIN));
        assert!(paths.contains(&routes::js_resources::CUSTOM_ELEMENTS));

        // Verify CSS path is present
        assert!(paths.contains(&routes::css_resources::STYLES));
    }
}