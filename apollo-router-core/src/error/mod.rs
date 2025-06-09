//! Error handling for Apollo Router Core
//!
//! This module re-exports error handling functionality from apollo-router-error
//! and provides integration with the core router architecture.

// Re-export error trait and GraphQL types from apollo-router-error
pub use apollo_router_error::{
    Error, ToGraphQLError, GraphQLError, GraphQLErrorContext, GraphQLErrorExtensions,
    GraphQLErrorLocation, GraphQLPathSegment, GraphQLErrorContextBuilder,
    get_registered_errors, get_registered_graphql_handlers,
    get_error_by_code, get_error_by_variant_code,
    get_errors_by_component, get_errors_by_category,
    get_all_error_codes, get_error_stats,
    export_error_registry_json, ErrorStats, ErrorRegistryEntry, ErrorVariantInfo
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_to_graphql_error_trait_available() {
        // Test that the ToGraphQLError trait is available and works
        let io_error = io::Error::new(io::ErrorKind::NotFound, "Test error");
        let graphql_error = io_error.as_graphql_error();
        
        assert_eq!(graphql_error.message, "Test error");
        assert_eq!(graphql_error.extensions.code, "APOLLO_ROUTER_UNKNOWN_ERROR");
    }

    #[test]
    fn test_error_registry_access() {
        // Test that error registry functions are accessible
        let all_errors = get_registered_errors();
        let stats = get_error_stats();
        
        assert!(all_errors.len() >= 0);
        assert!(stats.total_error_types >= 0);
    }
}
