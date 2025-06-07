//! Example demonstrating GraphQL error format conversion
//! 
//! This example shows how Apollo Router Core errors can be converted to standard
//! GraphQL error format with documented extensions.

use apollo_router_core::{
    CoreError, LayerError, GraphQLErrorContext, Error
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Apollo Router Core - GraphQL Error Format Demo\n");

    // Example 1: Basic error conversion
    println!("=== Example 1: Basic Query Parse Error ===");
    let parse_error = CoreError::QueryParseSyntax {
        reason: "Missing closing brace".to_string(),
        query_source: Some("query { user { name ".to_string()),
        error_span: Some((20, 1).into()),
    };

    let graphql_error = parse_error.to_graphql_error();
    println!("{}\n", serde_json::to_string_pretty(&graphql_error)?);

    // Example 2: Error with context
    println!("=== Example 2: Error with GraphQL Context ===");
    let timeout_error = CoreError::ExecutionTimeout {
        timeout_ms: 5000,
        service_name: "user-service".to_string(),
    };

    let context = GraphQLErrorContext::builder()
        .service_name("query-execution")
        .trace_id("trace-abc123")
        .request_id("req-456789")
        .location(3, 15)
        .path_field("user")
        .path_field("profile")
        .path_index(0)
        .build();

    let graphql_error_with_context = timeout_error.to_graphql_error_with_context(context);
    println!("{}\n", serde_json::to_string_pretty(&graphql_error_with_context)?);

    // Example 3: Layer conversion error
    println!("=== Example 3: Layer Conversion Error ===");
    let json_err = serde_json::from_str::<serde_json::Value>("{ invalid json")
        .unwrap_err();
    
    let layer_error = LayerError::BytesToJsonConversion {
        json_error: json_err,
        input_data: Some("{ invalid json data that is too long".to_string()),
        error_position: Some((2, 5).into()),
    };

    let layer_graphql_error = layer_error.to_graphql_error();
    println!("{}\n", serde_json::to_string_pretty(&layer_graphql_error)?);

    // Example 4: Network error
    println!("=== Example 4: Network Error ===");
    let network_error = CoreError::NetworkError(
        std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused")
    );

    let network_context = GraphQLErrorContext::builder()
        .service_name("http-client")
        .trace_id("trace-network-001")
        .build();

    let network_graphql_error = network_error.to_graphql_error_with_context(network_context);
    println!("{}\n", serde_json::to_string_pretty(&network_graphql_error)?);

    // Example 5: Multiple errors (as would appear in a GraphQL response)
    println!("=== Example 5: Multiple Errors in GraphQL Response Format ===");
    let errors = vec![
        parse_error.to_graphql_error(),
        timeout_error.to_graphql_error(),
    ];

    let graphql_response = serde_json::json!({
        "data": null,
        "errors": errors
    });

    println!("{}\n", serde_json::to_string_pretty(&graphql_response)?);

    println!("=== Key Features Demonstrated ===");
    println!("✅ Machine-readable error codes (apollo_router::* format)");
    println!("✅ Standard GraphQL error format compliance");
    println!("✅ Rich diagnostic information in extensions");
    println!("✅ Optional tracing and request correlation");
    println!("✅ Error-specific details for debugging");
    println!("✅ Proper JSON serialization/deserialization");

    Ok(())
} 