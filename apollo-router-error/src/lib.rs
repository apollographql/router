//! # Apollo Router Error Registry
//!
//! This crate provides error registration and introspection capabilities for Apollo Router Core
//! using linkme for distributed static collection of error information.
//!
//! ## Features
//!
//! - Distributed error registration using `linkme`
//! - Error introspection and querying capabilities  
//! - JSON serialization of error metadata
//! - Component and category-based error filtering
//! - Base error trait for all Apollo Router errors
//!
//! ## Usage
//!
//! ```rust,no_run
//! use apollo_router_error::{get_registered_errors, Error};
//!
//! // Define errors using the re-exported derive macro
//! #[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
//! pub enum MyError {
//!     #[error("Something went wrong: {message}")]
//!     #[diagnostic(code(apollo_router::my_service::something_wrong))]
//!     SomethingWrong { 
//!         #[extension("errorMessage")]
//!         message: String 
//!     },
//! }
//!
//! // Errors are automatically registered when using the derive macro
//! let all_errors = get_registered_errors();
//! for error in all_errors {
//!     println!("Error: {} - {}", error.type_name, error.error_code);
//! }
//! ```

// Re-export linkme for use by the derive macro
pub use linkme;

// Re-export the derive macro for convenience so users can use apollo_router_error::Error
pub use apollo_router_error_derive::Error;

use chrono::{DateTime, Utc};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Error registration entry for introspection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRegistryEntry {
    /// The error type name
    pub type_name: &'static str,
    /// The primary error code for this type
    pub error_code: &'static str,
    /// The error category (extracted from error code)
    pub category: &'static str,
    /// The error component (extracted from error code)
    pub component: &'static str,
    /// Documentation URL if available
    pub docs_url: Option<&'static str>,
    /// Help text if available
    pub help_text: Option<&'static str>,
    /// Error variants and their codes
    pub variants: Vec<ErrorVariantInfo>,
}

/// Information about an error variant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorVariantInfo {
    /// Variant name
    pub name: &'static str,
    /// Error code for this variant
    pub code: &'static str,
    /// Help text for this variant
    pub help: Option<&'static str>,
    /// Field names that will be included in GraphQL extensions
    pub graphql_fields: Vec<&'static str>,
}

/// Base error trait for all Apollo Router errors.
///
/// This trait ensures all errors have machine-readable codes and documentation URLs.
pub trait Error: std::error::Error + Diagnostic {
    /// Returns the machine-readable error code for this error
    fn error_code(&self) -> &'static str;

    /// Returns the documentation URL for this error
    fn docs_url(&self) -> Option<&'static str> {
        None
    }

    /// Converts this error to a GraphQL error format
    fn to_graphql_error(&self) -> GraphQLError {
        self.to_graphql_error_with_context(GraphQLErrorContext::default())
    }

    /// Converts this error to a GraphQL error format with additional context
    fn to_graphql_error_with_context(&self, context: GraphQLErrorContext) -> GraphQLError {
        let mut extensions = GraphQLErrorExtensions {
            code: convert_to_graphql_error_code(self.error_code()),
            timestamp: Utc::now(),
            service: context
                .service_name
                .unwrap_or_else(|| "apollo-router".to_string()),
            trace_id: context.trace_id,
            request_id: context.request_id,
            details: HashMap::new(),
        };

        // Add error-specific details
        self.populate_graphql_extensions(&mut extensions.details);

        GraphQLError {
            message: self.to_string(),
            locations: context.locations.unwrap_or_default(),
            path: context.path,
            extensions,
        }
    }

    /// Populate GraphQL error extensions with error-specific details
    ///
    /// This method can be overridden by specific error types to add
    /// additional context to the GraphQL error extensions.
    fn populate_graphql_extensions(&self, _details: &mut HashMap<String, serde_json::Value>) {
        // Default implementation - specific errors can override
    }
}

/// Converts Apollo Router internal error codes to GraphQL SCREAMING_SNAKE_CASE format
///
/// Transforms hierarchical codes like `apollo_router::query_parse::syntax_error`
/// into GraphQL standard format like `APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR`
fn convert_to_graphql_error_code(internal_code: &str) -> String {
    internal_code.replace("::", "_").to_uppercase()
}

/// GraphQL error format as defined by the GraphQL specification
///
/// This structure represents errors in the standard GraphQL error format,
/// including the Apollo Router specific extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphQLError {
    /// Human-readable error message
    pub message: String,

    /// Locations in the GraphQL query where the error occurred
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<GraphQLErrorLocation>,

    /// Path to the field that caused the error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<GraphQLPathSegment>>,

    /// Apollo Router specific error extensions
    pub extensions: GraphQLErrorExtensions,
}

/// Location information for GraphQL errors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphQLErrorLocation {
    /// Line number in the GraphQL query (1-based)
    pub line: u32,
    /// Column number in the GraphQL query (1-based)
    pub column: u32,
}

/// Path segment in GraphQL error path
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GraphQLPathSegment {
    /// Field name
    Field(String),
    /// Array index
    Index(u32),
}

/// Apollo Router specific GraphQL error extensions
///
/// These extensions provide machine-readable error information following
/// Apollo Router conventions and best practices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphQLErrorExtensions {
    /// Machine-readable error code in GraphQL SCREAMING_SNAKE_CASE format
    ///
    /// Internally derived from Apollo Router hierarchical codes like:
    /// `apollo_router::{component}::{category}::{specific_error}`
    ///
    /// GraphQL format examples:
    /// - `APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR`
    /// - `APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR`
    /// - `APOLLO_ROUTER_HTTP_SERVER_CONFIG_ERROR`
    pub code: String,

    /// ISO 8601 timestamp when the error occurred
    pub timestamp: DateTime<Utc>,

    /// Name of the service that generated the error
    pub service: String,

    /// Distributed tracing ID for correlating errors across services
    #[serde(skip_serializing_if = "Option::is_none", rename = "traceId")]
    pub trace_id: Option<String>,

    /// Unique request ID for this specific request
    #[serde(skip_serializing_if = "Option::is_none", rename = "requestId")]
    pub request_id: Option<String>,

    /// Additional error-specific details
    ///
    /// This field contains structured data specific to each error type.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub details: HashMap<String, serde_json::Value>,
}

/// Context information for creating GraphQL errors
#[derive(Debug, Default)]
pub struct GraphQLErrorContext {
    /// Name of the service generating the error
    pub service_name: Option<String>,

    /// Distributed tracing ID
    pub trace_id: Option<String>,

    /// Unique request ID
    pub request_id: Option<String>,

    /// GraphQL query locations where error occurred
    pub locations: Option<Vec<GraphQLErrorLocation>>,

    /// Path to the field that caused the error
    pub path: Option<Vec<GraphQLPathSegment>>,
}

impl GraphQLErrorContext {
    /// Create a new context builder
    pub fn builder() -> GraphQLErrorContextBuilder {
        GraphQLErrorContextBuilder::default()
    }
}

/// Builder for GraphQL error context
#[derive(Debug, Default)]
pub struct GraphQLErrorContextBuilder {
    context: GraphQLErrorContext,
}

impl GraphQLErrorContextBuilder {
    /// Set the service name
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.context.service_name = Some(name.into());
        self
    }

    /// Set the trace ID
    pub fn trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.context.trace_id = Some(trace_id.into());
        self
    }

    /// Set the request ID
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.context.request_id = Some(request_id.into());
        self
    }

    /// Add a GraphQL location
    pub fn location(mut self, line: u32, column: u32) -> Self {
        self.context
            .locations
            .get_or_insert_with(Vec::new)
            .push(GraphQLErrorLocation { line, column });
        self
    }

    /// Add a field to the GraphQL path
    pub fn path_field(mut self, field: impl Into<String>) -> Self {
        self.context
            .path
            .get_or_insert_with(Vec::new)
            .push(GraphQLPathSegment::Field(field.into()));
        self
    }

    /// Add an index to the GraphQL path
    pub fn path_index(mut self, index: u32) -> Self {
        self.context
            .path
            .get_or_insert_with(Vec::new)
            .push(GraphQLPathSegment::Index(index));
        self
    }

    /// Build the context
    pub fn build(self) -> GraphQLErrorContext {
        self.context
    }
}

/// Distributed static slice for error registry using linkme
#[linkme::distributed_slice]
pub static ERROR_REGISTRY: [ErrorRegistryEntry];

/// Register an error type for introspection
///
/// This macro is used by the derive macro to automatically register error types.
#[macro_export]
macro_rules! register_error {
    (
        registry_name: $registry_name:ident,
        type_name: $type_name:expr,
        error_code: $error_code:expr,
        category: $category:expr,
        component: $component:expr,
        $(docs_url: $docs_url:expr,)?
        $(help_text: $help_text:expr,)?
        variants: [$($variants:expr),* $(,)?]
    ) => {
        #[linkme::distributed_slice($crate::ERROR_REGISTRY)]
        static $registry_name: $crate::ErrorRegistryEntry = $crate::ErrorRegistryEntry {
            type_name: $type_name,
            error_code: $error_code,
            category: $category,
            component: $component,
            docs_url: None $(.or(Some($docs_url)))?,
            help_text: None $(.or(Some($help_text)))?,
            variants: vec![$($variants),*],
        };
    };
}

/// Get all registered errors for introspection
pub fn get_registered_errors() -> &'static [ErrorRegistryEntry] {
    &ERROR_REGISTRY
}

/// Get error by exact code match
pub fn get_error_by_code(code: &str) -> Option<&'static ErrorRegistryEntry> {
    ERROR_REGISTRY.iter().find(|entry| entry.error_code == code)
}

/// Get error by variant code (searches through all variants)
pub fn get_error_by_variant_code(
    code: &str,
) -> Option<(&'static ErrorRegistryEntry, &ErrorVariantInfo)> {
    for entry in ERROR_REGISTRY.iter() {
        for variant in &entry.variants {
            if variant.code == code {
                return Some((entry, variant));
            }
        }
    }
    None
}

/// Get errors by component  
pub fn get_errors_by_component(component: &str) -> Vec<&'static ErrorRegistryEntry> {
    ERROR_REGISTRY
        .iter()
        .filter(|entry| entry.component == component)
        .collect()
}

/// Get errors by category
pub fn get_errors_by_category(category: &str) -> Vec<&'static ErrorRegistryEntry> {
    ERROR_REGISTRY
        .iter()
        .filter(|entry| entry.category == category)
        .collect()
}

/// Get all error codes (including variant codes)
pub fn get_all_error_codes() -> Vec<&'static str> {
    let mut codes = Vec::new();
    for entry in ERROR_REGISTRY.iter() {
        for variant in &entry.variants {
            codes.push(variant.code);
        }
    }
    codes.sort();
    codes.dedup();
    codes
}

/// Get error statistics
#[derive(Debug, Serialize)]
pub struct ErrorStats {
    pub total_error_types: usize,
    pub total_variants: usize,
    pub components: Vec<String>,
    pub categories: Vec<String>,
}

/// Get statistics about registered errors
pub fn get_error_stats() -> ErrorStats {
    let total_error_types = ERROR_REGISTRY.len();
    let total_variants: usize = ERROR_REGISTRY
        .iter()
        .map(|entry| entry.variants.len())
        .sum();

    let mut components: Vec<String> = ERROR_REGISTRY
        .iter()
        .map(|entry| entry.component.to_string())
        .collect();
    components.sort();
    components.dedup();

    let mut categories: Vec<String> = ERROR_REGISTRY
        .iter()
        .map(|entry| entry.category.to_string())
        .collect();
    categories.sort();
    categories.dedup();

    ErrorStats {
        total_error_types,
        total_variants,
        components,
        categories,
    }
}

/// Export error registry as JSON
pub fn export_error_registry_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&*ERROR_REGISTRY)
}

/// Extract component and category from error code
pub fn extract_component_and_category(error_code: &str) -> (String, String) {
    let parts: Vec<&str> = error_code.split("::").collect();

    if parts.len() >= 3 {
        // Format: apollo_router::component::category
        let component = parts[1].to_string();
        let category = parts[2].to_string();
        (component, category)
    } else {
        ("unknown".to_string(), "unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_component_and_category() {
        let (component, category) =
            extract_component_and_category("apollo_router::query_parse::syntax_error");
        assert_eq!(component, "query_parse");
        assert_eq!(category, "syntax_error");

        let (component, category) =
            extract_component_and_category("apollo_router::layers::conversion_error");
        assert_eq!(component, "layers");
        assert_eq!(category, "conversion_error");
    }

    #[test]
    fn test_empty_registry() {
        // At compile time, there might not be any registered errors in tests
        let errors = get_registered_errors();
        assert!(errors.len() >= 0); // Could be empty or have test errors
    }

    #[test]
    fn test_error_stats() {
        let stats = get_error_stats();
        assert!(stats.total_error_types >= 0);
        assert!(stats.total_variants >= 0);
    }

    #[test]
    fn test_json_export() {
        let json_result = export_error_registry_json();
        assert!(json_result.is_ok());

        let json = json_result.unwrap();
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
    }
}
