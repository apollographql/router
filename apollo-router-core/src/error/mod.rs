//! Error handling for Apollo Router Core
//!
//! This module re-exports error handling functionality from apollo-router-error
//! and provides integration with the core router architecture.

// Re-export error trait and GraphQL types from apollo-router-error
pub use apollo_router_error::Error;
pub use apollo_router_error::ErrorRegistryEntry;
pub use apollo_router_error::ErrorStats;
pub use apollo_router_error::ErrorVariantInfo;
pub use apollo_router_error::GraphQLError;
pub use apollo_router_error::GraphQLErrorContext;
pub use apollo_router_error::GraphQLErrorContextBuilder;
pub use apollo_router_error::GraphQLErrorExtensions;
pub use apollo_router_error::GraphQLErrorLocation;
pub use apollo_router_error::GraphQLPathSegment;
pub use apollo_router_error::ToGraphQLError;
pub use apollo_router_error::export_error_registry_json;
pub use apollo_router_error::get_all_error_codes;
pub use apollo_router_error::get_error_by_code;
pub use apollo_router_error::get_error_by_variant_code;
pub use apollo_router_error::get_error_stats;
pub use apollo_router_error::get_errors_by_category;
pub use apollo_router_error::get_errors_by_component;
pub use apollo_router_error::get_registered_errors;
pub use apollo_router_error::get_registered_graphql_handlers;

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn test_to_graphql_error_trait_available() {
        // Test that the ToGraphQLError trait is available and works
        let io_error = io::Error::new(io::ErrorKind::NotFound, "Test error");
        let graphql_error = io_error.to_graphql_error();

        assert_eq!(graphql_error.message, "Test error");
        assert_eq!(graphql_error.extensions.code, "INTERNAL_ERROR");
    }

    #[test]
    fn test_error_registry_access() {
        // Test that error registry functions are accessible
        let all_errors = get_registered_errors();
        let stats = get_error_stats();

        // Check that we can access the registry (no need to check >= 0 for unsigned integers)
        assert!(all_errors.len() < 1000); // Sanity check
        assert!(stats.total_error_types < 1000); // Sanity check
    }
}
