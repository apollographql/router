# Apollo Router Error Registry and Derive Macro System

## Overview

We have successfully created a comprehensive error handling system for Apollo Router Core that provides:

✅ **Automatic Error Registration** using `linkme` for distributed static collection  
✅ **Derive Macro** that generates RouterError implementations automatically  
✅ **Runtime Introspection** of all error types with filtering and querying  
✅ **Structured Documentation** generation in JSON and Markdown formats  
✅ **GraphQL Integration** with automatic extensions population  
✅ **Zero-Configuration** - no manual registration required  

## System Architecture

### 1. Error Registry (`apollo-router-error`)

**Location**: `apollo-router-error/`  
**Purpose**: Distributed error collection and introspection using `linkme`

**Key Features**:
- Static distributed collection of error metadata using `linkme`
- Runtime querying by component, category, error code
- JSON export for tooling integration
- Statistics and analytics
- Type-safe error information storage

**Main API**:
```rust
// Get all registered errors
let all_errors = get_registered_errors();

// Filter by component
let service_errors = get_errors_by_component("query_parse");

// Find specific error by code
let error = get_error_by_variant_code("apollo_router::query_parse::syntax_error");

// Export for tooling
let json = export_error_registry_json()?;
```

### 2. Derive Macro (`apollo-router-error-derive`)

**Location**: `apollo-router-error-derive/`  
**Purpose**: Procedural macro that automates RouterError implementation

**Key Features**:
- Extracts error codes from `#[diagnostic(code(...))]` attributes
- Generates `error_code()` method automatically  
- Generates `populate_graphql_extensions()` method with field mapping
- Automatically registers errors with the global registry
- Infers GraphQL error types from error code patterns
- Handles special field attributes (`#[source]`, `#[source_code]`, `#[from]`)

**Generated Code Example**:
```rust
#[derive(Error, Diagnostic, Debug, RouterError)]
pub enum MyError {
    #[diagnostic(code(apollo_router::service::config_error))]
    ConfigError { message: String },
}

// Generates:
impl RouterError for MyError {
    fn error_code(&self) -> &'static str { /* ... */ }
    fn populate_graphql_extensions(&self, details: &mut HashMap<String, serde_json::Value>) { /* ... */ }
}

// Plus automatic registry entry
```

### 3. Core Integration (`apollo-router-core/src/error.rs`)

**Purpose**: Integration point with existing error handling infrastructure

**Key Features**:
- Re-exports derive macro and registry functions
- Provides `RouterError` trait definition
- GraphQL error format conversion
- Error context builder for rich error information

## Error Code Hierarchy

All error codes follow a strict hierarchical format:

```
apollo_router::{component}::{category}::{specific_error}
```

**Examples**:
- `apollo_router::query_parse::syntax_error`
- `apollo_router::layers::bytes_to_json::conversion_error`  
- `apollo_router::http_server::config_error`
- `apollo_router::execution::timeout`

This hierarchy enables:
- **Component-based filtering**: Find all errors for a specific service
- **Category classification**: Group errors by type (syntax, config, network, etc.)
- **Automatic documentation**: Generate component-specific error documentation
- **GraphQL error type inference**: Automatically determine error types for extensions

## Usage Examples

### Basic Error Definition

```rust
use apollo_router_error_derive::RouterError;
use thiserror::Error;
use miette::Diagnostic;

#[derive(Error, Diagnostic, Debug, RouterError)]
pub enum MyServiceError {
    #[error("Configuration failed: {message}")]
    #[diagnostic(
        code(apollo_router::my_service::config_error),
        help("Check your configuration parameters")
    )]
    ConfigError {
        message: String,
        #[source_code]
        config_source: Option<String>,
    },

    #[error("Network request failed")]
    #[diagnostic(code(apollo_router::my_service::network_error))]
    NetworkError(#[from] std::io::Error),
}
```

### Runtime Introspection

```rust
use apollo_router_error::*;

// Get statistics
let stats = get_error_stats();
println!("Total error types: {}", stats.total_error_types);

// Filter by component
let query_errors = get_errors_by_component("query_parse");

// Export documentation
let json = export_error_registry_json()?;
std::fs::write("error_docs.json", json)?;
```

### GraphQL Integration

```rust
let error = MyServiceError::ConfigError {
    message: "Invalid port".to_string(),
    config_source: Some("port: invalid".to_string()),
};

let context = GraphQLErrorContext::builder()
    .service_name("my-service")
    .trace_id("trace-123")
    .build();

let graphql_error = error.to_graphql_error_with_context(context);
// Result includes machine-readable error codes, structured details, timestamps
```

## File Structure

```
apollo-router-error/
├── src/lib.rs                 # Registry implementation with linkme
├── Cargo.toml                 # Dependencies: linkme, serde, serde_json

apollo-router-error-derive/
├── src/lib.rs                 # Derive macro implementation  
├── README.md                  # Comprehensive usage documentation
├── Cargo.toml                 # Dependencies: syn, quote, proc-macro2

apollo-router-core/
├── src/error.rs               # Core error infrastructure + re-exports
├── Cargo.toml                 # Updated with error dependencies

examples/
├── error_registry_demo.rs     # Complete working example

ERROR_REGISTRY_SYSTEM.md       # This overview document
```

## Key Innovations

### 1. Zero-Configuration Registration
Using `linkme`, errors are automatically registered when the derive macro is used. No manual registration calls needed.

### 2. Compile-Time Error Code Extraction
The derive macro parses `#[diagnostic]` attributes at compile time to extract error codes, ensuring consistency and preventing runtime errors.

### 3. Automatic GraphQL Extensions
Field-to-extension mapping is generated automatically based on field names and types, with special handling for `#[source]` and `#[source_code]` fields.

### 4. Hierarchical Error Organization
The error code format enables powerful filtering and organization capabilities while maintaining semantic meaning.

### 5. Documentation Generation
Both JSON and Markdown documentation can be generated automatically from the registry for tooling integration and human consumption.

## Benefits

### For Developers
- **Reduced Boilerplate**: No manual RouterError implementations needed
- **Consistency**: Error codes extracted from existing diagnostic attributes
- **Type Safety**: Compile-time validation of error structures
- **Rich Diagnostics**: Automatic GraphQL extensions with structured data

### For Operations
- **Runtime Introspection**: Query and analyze all error types at runtime
- **Automated Documentation**: Generate comprehensive error documentation
- **Component Filtering**: Find all errors for specific services or layers
- **Tooling Integration**: JSON export for monitoring and alerting systems

### For API Consumers
- **Machine-Readable Codes**: Consistent error code format across all services
- **Structured Details**: Rich error context in GraphQL extensions
- **Standardized Format**: All errors follow the same GraphQL error specification

## Implementation Status

✅ **Complete**: Error registry with linkme-based collection  
✅ **Complete**: Derive macro with error code extraction and generation  
✅ **Complete**: GraphQL extensions generation  
✅ **Complete**: Component/category-based filtering  
✅ **Complete**: JSON and Markdown documentation export  
✅ **Complete**: Integration with apollo-router-core  
✅ **Complete**: Comprehensive example and documentation  

## Future Enhancements

### Potential Improvements
1. **IDE Integration**: Error code lookup and documentation in IDEs
2. **Monitoring Integration**: Automatic error metrics and alerting
3. **Error Analytics**: Pattern analysis and error frequency tracking
4. **Localization**: Multi-language error messages and help text
5. **Interactive Documentation**: Web-based error code explorer

### Extension Points
- Custom GraphQL extensions generators
- Additional output formats (OpenAPI, etc.)  
- Error code validation rules
- Custom introspection queries
- Integration with external documentation systems

## Testing Strategy

The system includes comprehensive testing:

- **Unit Tests**: Individual component functionality
- **Integration Tests**: End-to-end error registration and lookup
- **Example Tests**: Verification that the demo example works correctly
- **Compile-Time Tests**: Error code format validation
- **GraphQL Tests**: Extensions generation and format compliance

## Performance Considerations

- **Compile-Time Work**: Most complexity is handled at compile time
- **Static Storage**: Registry uses static memory with minimal runtime overhead
- **Lazy Evaluation**: Registry is only populated when errors are first defined
- **Zero-Cost Abstractions**: No runtime performance impact for unused features

## Summary

This error registry and derive macro system provides a comprehensive, zero-configuration solution for error handling in Apollo Router Core. It eliminates boilerplate, ensures consistency, enables powerful introspection capabilities, and provides excellent integration with GraphQL error standards.

The system is production-ready and provides a solid foundation for scalable error handling across the entire Apollo Router ecosystem. 