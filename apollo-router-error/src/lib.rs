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
//! - Universal GraphQL error conversion through error registry and downcasting
//!
//! ## Usage
//!
//! ```rust,no_run
//! use apollo_router_error::{get_registered_errors, Error, ToGraphQLError, HeapErrorToGraphQL};
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
//!
//! // Convert any error to GraphQL error (even if not Apollo Router error)
//! let std_error = std::io::Error::new(std::io::ErrorKind::NotFound, "File not found");
//! let graphql_error = std_error.to_graphql_error();
//!
//! // Convert boxed/arc errors using consistent API
//! let box_error: Box<dyn std::error::Error + Send + Sync> = Box::new(std_error);
//! let graphql_error = HeapErrorToGraphQL::to_graphql_error(&box_error);
//!
//! // Alternative: use standalone functions
//! let graphql_error = apollo_router_error::box_to_graphql_error(&box_error);
//! ```

// Re-export linkme for use by the derive macro
use std::collections::BTreeMap;
use std::sync::Arc;

// Re-export the derive macro for convenience so users can use apollo_router_error::Error
pub use apollo_router_error_derive::Error;
use chrono::DateTime;
use chrono::Utc;
pub use linkme;
use miette::Diagnostic;
use serde::Deserialize;
use serde::Serialize;

/// Error registration entry for introspection
#[derive(Debug, Clone)]
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
    pub variants: &'static [ErrorVariantInfo],
}

/// Information about an error variant
#[derive(Debug, Clone)]
pub struct ErrorVariantInfo {
    /// Variant name
    pub name: &'static str,
    /// Error code for this variant
    pub code: &'static str,
    /// Help text for this variant
    pub help: Option<&'static str>,
    /// Field names that will be included in GraphQL extensions
    pub graphql_fields: &'static [&'static str],
}

/// Serializable version of ErrorRegistryEntry for JSON export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableErrorRegistryEntry {
    /// The error type name
    pub type_name: String,
    /// The primary error code for this type
    pub error_code: String,
    /// The error category (extracted from error code)
    pub category: String,
    /// The error component (extracted from error code)
    pub component: String,
    /// Documentation URL if available
    pub docs_url: Option<String>,
    /// Help text if available
    pub help_text: Option<String>,
    /// Error variants and their codes
    pub variants: Vec<SerializableErrorVariantInfo>,
}

/// Serializable version of ErrorVariantInfo for JSON export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableErrorVariantInfo {
    /// Variant name
    pub name: String,
    /// Error code for this variant
    pub code: String,
    /// Help text for this variant
    pub help: Option<String>,
    /// Field names that will be included in GraphQL extensions
    pub graphql_fields: Vec<String>,
}

impl From<&ErrorRegistryEntry> for SerializableErrorRegistryEntry {
    fn from(entry: &ErrorRegistryEntry) -> Self {
        Self {
            type_name: entry.type_name.to_string(),
            error_code: entry.error_code.to_string(),
            category: entry.category.to_string(),
            component: entry.component.to_string(),
            docs_url: entry.docs_url.map(|s| s.to_string()),
            help_text: entry.help_text.map(|s| s.to_string()),
            variants: entry
                .variants
                .iter()
                .map(SerializableErrorVariantInfo::from)
                .collect(),
        }
    }
}

impl From<&ErrorVariantInfo> for SerializableErrorVariantInfo {
    fn from(variant: &ErrorVariantInfo) -> Self {
        Self {
            name: variant.name.to_string(),
            code: variant.code.to_string(),
            help: variant.help.map(|s| s.to_string()),
            graphql_fields: variant
                .graphql_fields
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Type alias for a GraphQL error conversion handler function
///
/// This function takes a `dyn std::error::Error + 'static` and attempts to downcast it
/// to a specific error type. If successful, it converts it to a GraphQL error.
pub type GraphQLErrorHandler =
    fn(&(dyn std::error::Error + 'static), GraphQLErrorContext) -> Option<GraphQLError>;

/// GraphQL error conversion handler entry
pub struct GraphQLErrorHandlerEntry {
    /// The error type name this handler supports
    pub type_name: &'static str,
    /// The conversion function
    pub handler: GraphQLErrorHandler,
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
            details: BTreeMap::new(),
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
    fn populate_graphql_extensions(
        &self,
        _extensions_map: &mut BTreeMap<String, serde_json::Value>,
    ) {
        // Default implementation - specific errors can override
    }
}

/// Universal trait for converting any error to GraphQL format
///
/// This trait is automatically implemented for all types that implement `std::error::Error`
/// and provides a unified interface for converting errors to GraphQL format.
///
/// The trait uses the error registry to attempt downcasting to known Apollo Router error types
/// first, falling back to a generic GraphQL error if no specific handler is found.
pub trait ToGraphQLError {
    /// Converts this error to a GraphQL error format using the error registry
    fn to_graphql_error(&self) -> GraphQLError {
        self.to_graphql_error_with_context(GraphQLErrorContext::default())
    }

    /// Converts this error to a GraphQL error format with additional context using the error registry
    fn to_graphql_error_with_context(&self, context: GraphQLErrorContext) -> GraphQLError;
}

#[allow(clippy::borrowed_box)]
pub fn box_to_graphql_error(error: &Box<dyn std::error::Error + Send + Sync>) -> GraphQLError {
    box_to_graphql_error_with_context(error, GraphQLErrorContext::default())
}

pub fn arc_to_graphql_error(error: &Arc<dyn std::error::Error + Send + Sync>) -> GraphQLError {
    arc_to_graphql_error_with_context(error, GraphQLErrorContext::default())
}

/// Convert a boxed error to GraphQL format with context
#[allow(clippy::borrowed_box)]
pub fn box_to_graphql_error_with_context(
    error: &Box<dyn std::error::Error + Send + Sync>,
    context: GraphQLErrorContext,
) -> GraphQLError {
    let error_ref: &dyn std::error::Error = error.as_ref();

    // First try to unwrap nested wrapper types recursively
    if let Some(nested_arc) = error_ref.downcast_ref::<Arc<dyn std::error::Error + Send + Sync>>() {
        return arc_to_graphql_error_with_context(nested_arc, context);
    }

    // Try to convert using registered handlers
    for handler_entry in GRAPHQL_ERROR_HANDLERS.iter() {
        if let Some(graphql_error) = (handler_entry.handler)(error_ref, context.clone()) {
            return graphql_error;
        }
    }

    // Fall back to generic GraphQL error
    create_generic_graphql_error(error_ref, context)
}

/// Convert an arc error to GraphQL format with context
pub fn arc_to_graphql_error_with_context(
    error: &Arc<dyn std::error::Error + Send + Sync>,
    context: GraphQLErrorContext,
) -> GraphQLError {
    let error_ref: &dyn std::error::Error = error.as_ref();

    // Try to convert using registered handlers
    for handler_entry in GRAPHQL_ERROR_HANDLERS.iter() {
        if let Some(graphql_error) = (handler_entry.handler)(error_ref, context.clone()) {
            return graphql_error;
        }
    }

    // Fall back to generic GraphQL error
    create_generic_graphql_error(error_ref, context)
}

/// Extension trait providing consistent GraphQL error conversion for heap-allocated wrapper types
///
/// This trait provides a consistent API for converting heap-allocated errors (Box and Arc)
/// to GraphQL format, using the same method names as the ToGraphQLError trait.
pub trait HeapErrorToGraphQL {
    /// Converts this boxed/arc error to a GraphQL error format
    fn to_graphql_error(&self) -> GraphQLError {
        self.to_graphql_error_with_context(GraphQLErrorContext::default())
    }

    /// Converts this boxed/arc error to a GraphQL error format with additional context
    fn to_graphql_error_with_context(&self, context: GraphQLErrorContext) -> GraphQLError;
}

/// Implementation for Box<dyn Error + Send + Sync>
impl HeapErrorToGraphQL for Box<dyn std::error::Error + Send + Sync> {
    fn to_graphql_error_with_context(&self, context: GraphQLErrorContext) -> GraphQLError {
        box_to_graphql_error_with_context(self, context)
    }
}

/// Implementation for Arc<dyn Error + Send + Sync>  
impl HeapErrorToGraphQL for Arc<dyn std::error::Error + Send + Sync> {
    fn to_graphql_error_with_context(&self, context: GraphQLErrorContext) -> GraphQLError {
        arc_to_graphql_error_with_context(self, context)
    }
}

/// Blanket implementation of ToGraphQLError for all std::error::Error types
impl<T: std::error::Error + Send + Sync + 'static> ToGraphQLError for T {
    fn to_graphql_error_with_context(&self, context: GraphQLErrorContext) -> GraphQLError {
        // Try to convert using registered handlers first
        for handler_entry in GRAPHQL_ERROR_HANDLERS.iter() {
            if let Some(graphql_error) =
                (handler_entry.handler)(self as &dyn std::error::Error, context.clone())
            {
                return graphql_error;
            }
        }

        // Fall back to generic GraphQL error
        create_generic_graphql_error(self as &dyn std::error::Error, context)
    }
}

/// Creates a generic GraphQL error for unknown error types
fn create_generic_graphql_error(
    error: &dyn std::error::Error,
    context: GraphQLErrorContext,
) -> GraphQLError {
    let mut extensions = GraphQLErrorExtensions {
        code: "INTERNAL_ERROR".to_string(),
        timestamp: Utc::now(),
        service: context
            .service_name
            .unwrap_or_else(|| "unknown".to_string()),
        trace_id: context.trace_id,
        request_id: context.request_id,
        details: BTreeMap::new(),
    };

    // Add the error type information
    extensions.details.insert(
        "errorType".to_string(),
        serde_json::Value::String(std::any::type_name_of_val(error).to_string()),
    );

    // Include error chain information
    let mut error_chain = Vec::new();
    let mut current = Some(error);
    while let Some(err) = current {
        error_chain.push(err.to_string());
        current = err.source();
    }

    if error_chain.len() > 1 {
        extensions.details.insert(
            "errorChain".to_string(),
            serde_json::to_value(error_chain).unwrap_or(serde_json::Value::Null),
        );
    }

    GraphQLError {
        message: error.to_string(),
        locations: context.locations.unwrap_or_default(),
        path: context.path,
        extensions,
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

/// Context information for creating GraphQL errors
#[derive(Debug, Default, Clone)]
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

/// Distributed static slice for GraphQL error handlers using linkme
#[linkme::distributed_slice]
pub static GRAPHQL_ERROR_HANDLERS: [GraphQLErrorHandlerEntry];

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
        #[$crate::linkme::distributed_slice($crate::ERROR_REGISTRY)]
        static $registry_name: $crate::ErrorRegistryEntry = $crate::ErrorRegistryEntry {
            type_name: $type_name,
            error_code: $error_code,
            category: $category,
            component: $component,
            docs_url: None $(.or(Some($docs_url)))?,
            help_text: None $(.or(Some($help_text)))?,
            variants: &[$($variants),*],
        };
    };
}

/// Register a GraphQL error conversion handler
///
/// This macro is used by the derive macro to automatically register GraphQL error handlers.
#[macro_export]
macro_rules! register_graphql_error_handler {
    (
        handler_name: $handler_name:ident,
        static_name: $static_name:ident,
        type_name: $type_name:expr,
        error_type: $error_type:ty
    ) => {
        fn $handler_name(
            error: &(dyn std::error::Error + 'static),
            context: $crate::GraphQLErrorContext,
        ) -> Option<$crate::GraphQLError> {
            // Try direct downcast first
            if let Some(typed_error) = error.downcast_ref::<$error_type>() {
                use $crate::Error as RouterError;
                return Some(RouterError::to_graphql_error_with_context(
                    typed_error,
                    context,
                ));
            }

            // Try downcast through Box<T>
            if let Some(boxed_error) = error.downcast_ref::<Box<$error_type>>() {
                use $crate::Error as RouterError;
                return Some(RouterError::to_graphql_error_with_context(
                    boxed_error.as_ref(),
                    context,
                ));
            }

            // Try downcast through Arc<T>
            if let Some(arc_error) = error.downcast_ref::<std::sync::Arc<$error_type>>() {
                use $crate::Error as RouterError;
                return Some(RouterError::to_graphql_error_with_context(
                    arc_error.as_ref(),
                    context,
                ));
            }

            // Try downcast through Box<Arc<T>>
            if let Some(box_arc_error) = error.downcast_ref::<Box<std::sync::Arc<$error_type>>>() {
                use $crate::Error as RouterError;
                return Some(RouterError::to_graphql_error_with_context(
                    box_arc_error.as_ref().as_ref(),
                    context,
                ));
            }

            None
        }

        #[$crate::linkme::distributed_slice($crate::GRAPHQL_ERROR_HANDLERS)]
        static $static_name: $crate::GraphQLErrorHandlerEntry = $crate::GraphQLErrorHandlerEntry {
            type_name: $type_name,
            handler: $handler_name,
        };
    };
}

/// Get all registered errors for introspection
pub fn get_registered_errors() -> &'static [ErrorRegistryEntry] {
    &ERROR_REGISTRY
}

/// Get all registered GraphQL error handlers
pub fn get_registered_graphql_handlers() -> &'static [GraphQLErrorHandlerEntry] {
    &GRAPHQL_ERROR_HANDLERS
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
        for variant in entry.variants.iter() {
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
        for variant in entry.variants.iter() {
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
    pub total_graphql_handlers: usize,
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
    let total_graphql_handlers = GRAPHQL_ERROR_HANDLERS.len();

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
        total_graphql_handlers,
        components,
        categories,
    }
}

/// Export error registry as JSON
pub fn export_error_registry_json() -> Result<String, serde_json::Error> {
    let serializable: Vec<SerializableErrorRegistryEntry> = ERROR_REGISTRY
        .iter()
        .map(SerializableErrorRegistryEntry::from)
        .collect();
    serde_json::to_string_pretty(&serializable)
}
