# Apollo Router Core Error Handling Strategy

## Overview

Apollo Router Core implements a comprehensive error handling strategy using **miette + thiserror** that provides:

- ✅ **Machine-readable error codes** for every error
- ✅ **Compile-time verification** of error codes and documentation
- ✅ **Rich diagnostic output** with source code snippets
- ✅ **Automatic documentation links** to docs.rs
- ✅ **JSON output support** for machine processing
- ✅ **Contextual help text** for error resolution

## Core Components

### 1. Base Error Trait

All errors implement the `Error` trait from `apollo-router-error` which ensures consistency:

```rust
pub trait Error: std::error::Error + Diagnostic {
    fn error_code(&self) -> &'static str;
    fn docs_url(&self) -> Option<&'static str>;
    fn to_graphql_error(&self) -> GraphQLError;
    fn populate_graphql_extensions(&self, details: &mut HashMap<String, serde_json::Value>);
}
```

### 2. Derive Macro and Re-exports

For convenience, the `Error` derive macro is re-exported from `apollo-router-error`:

```rust
use apollo_router_error::Error; // Gets both the trait AND the derive macro

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum MyError {
    // ... error variants
}
```

### 3. Hierarchical Error Types

#### CoreError
Main error enum for service-level errors with comprehensive error codes:

```rust
use apollo_router_error::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum CoreError {
    #[error("GraphQL query parsing failed: {reason}")]
    #[diagnostic(
        code(apollo_router::query_parse::syntax_error),
        url(docsrs),
        help("Ensure your GraphQL query syntax is correct")
    )]
    QueryParseSyntax {
        #[extension("parseReason")]
        reason: String,
        #[source_code]
        query_source: Option<String>,
        #[label("Parse error")]
        error_span: Option<SourceSpan>,
    },
    // ... more variants
}
```

#### LayerError
Specific errors for transformation layers:

```rust
use apollo_router_error::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum LayerError {
    #[error("Bytes to JSON conversion failed")]
    #[diagnostic(
        code(apollo_router::layers::bytes_to_json::conversion_error),
        url(docsrs),
        help("Ensure the input is valid JSON")
    )]
    BytesToJsonConversion {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        input_data: Option<String>,
        #[label("Invalid JSON")]
        error_position: Option<SourceSpan>,
        #[extension("jsonErrorDetails")]
        error_details: String,
    },
    // ... more variants
}
```

#### GraphQL Extension Fields

Fields that should be included in GraphQL error extensions must be explicitly marked with the `#[extension]` attribute:

- **`#[extension("customName")]`** - Uses the specified custom name in the GraphQL extensions
- **`#[extension]`** - Uses the field name converted to camelCase (e.g., `error_code` → `"errorCode"`)

Only fields with explicit `#[extension]` attributes are included in GraphQL error extensions. This provides precise control over what information is exposed in error responses.

## Error Code Structure

All error codes follow a hierarchical naming convention:

```
apollo_router::{component}::{category}::{specific_error}
```

Examples:
- `apollo_router::query_parse::syntax_error`
- `apollo_router::layers::bytes_to_json::conversion_error`
- `apollo_router::http_server::config_error`
- `apollo_router::execution::timeout`

## Usage Examples

### Service Implementation

```rust
use apollo_router_core::error::{CoreError, Result};

pub fn parse_query(query: &str) -> Result<ExecutableDocument> {
    if query.is_empty() {
        return Err(CoreError::QueryParseSyntax {
            reason: "Query cannot be empty".to_string(),
            query_source: Some(query.to_string()),
            error_span: Some((0, 0).into()),
        });
    }
    
    // ... actual parsing logic
    Ok(parsed_document)
}
```

### Layer Implementation

```rust
use apollo_router_core::error::{LayerError, LayerResult};

pub fn convert_bytes_to_json(input: &[u8]) -> LayerResult<serde_json::Value> {
    match serde_json::from_slice(input) {
        Ok(value) => Ok(value),
        Err(json_err) => Err(LayerError::BytesToJsonConversion {
            json_error: json_err,
            input_data: String::from_utf8_lossy(input).into_owned().into(),
            error_position: Some((0, input.len()).into()),
            error_details: format!("JSON parsing failed: {}", json_err),
        })
    }
}
```

### Error Definition with Extension Control

```rust
use apollo_router_error::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum MyServiceError {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(apollo_router::my_service::config_error),
        help("Check your configuration file")
    )]
    ConfigError {
        #[extension("configMessage")]
        message: String,
        #[extension] // Will use camelCase: "configPath"
        config_path: String,
        #[source_code]
        config_source: Option<String>, // Not included in extensions
    },

    #[error("Network timeout after {timeout_ms}ms")]
    #[diagnostic(code(apollo_router::my_service::network_timeout))]
    NetworkTimeout {
        #[extension]
        timeout_ms: u64,
        #[extension("endpoint")]
        endpoint_url: String,
    },
}
```

### Application-Level Error Handling

For applications using Apollo Router Core:

```rust
use apollo_router_core::{IntoDiagnostic, Context};
use miette::Result;

fn main() -> Result<()> {
    let result = apollo_router_core::some_operation()
        .into_diagnostic()
        .wrap_err("Failed to initialize Apollo Router")?;
    
    Ok(())
}
```

## Rich Diagnostic Output

Errors provide rich diagnostic information:

```
Error: apollo_router::query_parse::syntax_error

  × GraphQL query parsing failed: Missing closing brace
  ╭─[query.graphql:1:1]
  1 │ query { user { name 
    ·                     ▲
    ·                     ╰── Parse error
  ╰────
  help: Ensure your GraphQL query syntax is correct

  For more details, visit: https://docs.rs/apollo_router_core/latest/apollo_router_core/error/enum.CoreError.html#variant.QueryParseSyntax
```

## Compile-Time Verification Features

### 1. Error Code Validation

All error codes are validated at compile time through the derive macro:

```rust
// This is generated automatically by the derive macro
impl apollo_router_error::Error for CoreError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::QueryParseSyntax { .. } => "apollo_router::query_parse::syntax_error",
            // Compiler ensures all variants are covered
        }
    }
    
    fn populate_graphql_extensions(&self, details: &mut HashMap<String, serde_json::Value>) {
        match self {
            Self::QueryParseSyntax { reason, .. } => {
                details.insert("errorType".to_string(), serde_json::Value::String("syntax".to_string()));
                details.insert("parseReason".to_string(), serde_json::to_value(reason).unwrap_or(serde_json::Value::Null));
            }
            // ... other variants
        }
    }
}
```

### 2. Documentation Link Generation

Using `url(docsrs)` automatically generates links to documentation:

```rust
#[diagnostic(
    code(apollo_router::query_parse::syntax_error),
    url(docsrs),  // Automatically links to docs.rs
    help("Ensure your GraphQL query syntax is correct")
)]
```

### 3. Type Safety

All errors are strongly typed with compile-time verification:

```rust
// Compile error if error code doesn't match pattern
#[test]
fn verify_error_codes() {
    assert_eq!(
        CoreError::QueryParseSyntax { /* ... */ }.error_code(),
        "apollo_router::query_parse::syntax_error"
    );
}
```

## Machine-Readable Output

For automation and monitoring:

### JSON Output
```rust
use miette::JSONReportHandler;

let error = CoreError::QueryParseSyntax { /* ... */ };
let json_output = format!("{:?}", miette::Report::new(error));
// Outputs structured JSON with error codes, messages, and metadata
```

### Structured Logging
```rust
use tracing::error;

if let Err(e) = operation() {
    error!(
        error_code = e.error_code(),
        error_message = %e,
        "Operation failed"
    );
}
```

## Integration with Existing Architecture

### Service Error Types

Each service should define its own error enum using the re-exported derive macro:

```rust
// In services/query_parse/mod.rs
use apollo_router_error::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum Error {
    #[error("Syntax error: {message}")]
    #[diagnostic(
        code(apollo_router::query_parse::syntax_error),
        url(docsrs)
    )]
    Syntax { 
        #[extension("syntaxMessage")]
        message: String,
        #[extension]
        line_number: u32,
    },
    
    #[error("Invalid operation: {operation}")]
    #[diagnostic(
        code(apollo_router::query_parse::invalid_operation),
        url(docsrs)
    )]
    InvalidOperation { 
        #[extension]
        operation: String 
    },
}
```

### Layer Error Conversion

Layers should convert downstream errors to rich diagnostic errors:

```rust
impl<S> Service<Request> for MyLayer<S> 
where 
    S: Service<Request, Error = SomeError>
{
    type Error = LayerError;
    
    fn call(&mut self, req: Request) -> Self::Future {
        let result = self.inner.call(req).await
            .map_err(|e| LayerError::ServiceComposition {
                phase: "request_processing".to_string(),
                service_name: "my_layer".to_string(),
                underlying_error: Box::new(e),
            })?;
        
        Ok(result)
    }
}
```

## Error Code Registry

Maintain a centralized registry of all error codes for documentation and tooling:

```rust
// Generated at compile time or maintained manually
pub static ERROR_CODE_REGISTRY: &[(&str, &str)] = &[
    ("apollo_router::query_parse::syntax_error", "GraphQL syntax parsing failed"),
    ("apollo_router::layers::bytes_to_json::conversion_error", "JSON conversion failed"),
    ("apollo_router::http_server::config_error", "HTTP server configuration error"),
    // ... more codes
];
```

## Testing Strategy

### Error Code Verification
```rust
#[test]
fn test_all_error_codes_are_documented() {
    for (code, _description) in ERROR_CODE_REGISTRY {
        // Verify each code has documentation
        assert!(docs_exist_for_code(code));
    }
}
```

### Rich Error Testing
```rust
#[test]
fn test_error_with_source_context() {
    let error = CoreError::QueryParseSyntax {
        reason: "Missing brace".to_string(),
        query_source: Some("query { user".to_string()),
        error_span: Some((11, 1).into()),
    };
    
    // Test that error includes source code context
    let report = miette::Report::new(error);
    let output = format!("{:?}", report);
    assert!(output.contains("query { user"));
    assert!(output.contains("Missing brace"));
}
```

## GraphQL Error Format Support

Apollo Router Core provides comprehensive support for converting errors to standard GraphQL error format with documented extensions.

### GraphQL Error Structure

All errors can be converted to the standard GraphQL error format:

```json
{
  "errors": [
    {
      "message": "GraphQL query parsing failed: Missing closing brace",
      "locations": [
        {
          "line": 3,
          "column": 15
        }
      ],
      "path": ["user", "profile", 0],
             "extensions": {
         "code": "APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR",
         "timestamp": "2024-01-15T10:30:00Z",
         "service": "query-parse",
         "traceId": "abc123",
         "requestId": "req-456",
         "details": {
           "errorType": "syntax",
           "reason": "Missing closing brace",
           "querySource": "query { user { name }",
           "hasPositionInfo": true
         }
       }
    }
  ]
}
```

### Basic Error Conversion

```rust
use apollo_router_core::error::{CoreError, RouterError};

let error = CoreError::QueryParseSyntax {
    reason: "Missing closing brace".to_string(),
    query_source: Some("query { user { name }".to_string()),
    error_span: Some((20, 1).into()),
};

// Convert to GraphQL error format
let graphql_error = error.to_graphql_error();
println!("{}", serde_json::to_string_pretty(&graphql_error)?);
```

### Error Conversion with Context

```rust
use apollo_router_core::error::{CoreError, GraphQLErrorContext, RouterError};

let error = CoreError::ExecutionTimeout {
    timeout_ms: 5000,
    service_name: "user-service".to_string(),
};

let context = GraphQLErrorContext::builder()
    .service_name("query-execution")
    .trace_id("trace-123")
    .request_id("req-456")
    .location(3, 15)
    .path_field("user")
    .path_field("profile")
    .path_index(0)
    .build();

let graphql_error = error.to_graphql_error_with_context(context);
```

### GraphQL Error Extensions Documentation

The `extensions` field contains Apollo Router Core specific information:

#### Error Code Format Conversion

Apollo Router Core uses hierarchical error codes internally (e.g., `apollo_router::query_parse::syntax_error`) but converts them to GraphQL SCREAMING_SNAKE_CASE format for the GraphQL `extensions.code` field:

- Internal: `apollo_router::query_parse::syntax_error` 
- GraphQL: `APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR`

- Internal: `apollo_router::layers::bytes_to_json::conversion_error`
- GraphQL: `APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR`

This conversion ensures compliance with GraphQL error code conventions while maintaining internal structure.

#### Required Fields

- **`code`**: Machine-readable error code in GraphQL SCREAMING_SNAKE_CASE format (e.g., `APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR`)
- **`timestamp`**: ISO 8601 timestamp when the error occurred
- **`service`**: Name of the service that generated the error

#### Optional Fields

- **`traceId`**: Distributed tracing ID for correlating errors across services
- **`requestId`**: Unique request ID for this specific request

#### Details Field

The `details` field contains structured, error-specific information:

##### Query Parse Errors
```json
{
  "errorType": "syntax",
  "parseReason": "Missing closing brace"
}
```

##### HTTP Server Config Errors
```json
{
  "errorType": "config",
  "configMessage": "Invalid port number",
  "configPath": "/etc/router.yaml"
}
```

##### Layer Conversion Errors
```json
{
  "errorType": "conversion",
  "jsonErrorDetails": "JSON parsing failed: expected value at line 1 column 1"
}
```

##### Network Errors
```json
{
  "errorType": "network",
  "ioKind": "ConnectionRefused"
}
```

##### Timeout Errors
```json
{
  "errorType": "timeout",
  "timeoutMs": 5000,
  "serviceName": "user-service"
}
```

### Service Integration

Services can easily provide GraphQL-compatible errors:

```rust
use apollo_router_core::error::{CoreError, GraphQLErrorContext, RouterError};
use tower::Service;

impl MyService {
    async fn handle_request(&self, req: Request) -> Result<Response> {
        let result = self.process_query(&req.query).await?;
        
        if let Err(parse_error) = result {
            // Convert to GraphQL error with context from request
            let context = GraphQLErrorContext::builder()
                .service_name("my-service")
                .trace_id(&req.extensions.get::<TraceId>())
                .request_id(&req.extensions.get::<RequestId>())
                .build();
                
            let graphql_error = parse_error.to_graphql_error_with_context(context);
            
            // Return as JSON response or propagate as needed
            return Ok(Response::error(graphql_error));
        }
        
        Ok(Response::success(result))
    }
}
```

### Testing GraphQL Error Conversion

```rust
#[test]
fn test_graphql_error_format() {
    let error = CoreError::QueryPlanningFailed {
        reason: "Schema not found".to_string(),
        operation_name: Some("GetUser".to_string()),
    };

    let graphql_error = error.to_graphql_error();

    // Verify standard GraphQL error format
    assert_eq!(graphql_error.message, "Query planning failed: Schema not found");
    assert_eq!(graphql_error.extensions.code, "APOLLO_ROUTER_QUERY_PLAN_PLANNING_ERROR");
    
    // Verify JSON serialization works
    let json = serde_json::to_string(&graphql_error).unwrap();
    let parsed: GraphQLError = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.extensions.code, graphql_error.extensions.code);
}
```

## Benefits

1. **Developer Experience**: Rich error messages with context and help text
2. **Monitoring**: Machine-readable error codes for automated systems
3. **Documentation**: Automatic links to relevant documentation
4. **Debugging**: Source code snippets and precise error locations
5. **Maintainability**: Compile-time verification ensures consistency
6. **Extensibility**: Easy to add new error types while maintaining structure
7. **GraphQL Compatibility**: Standard GraphQL error format with documented extensions
8. **Automation Support**: Structured error information for automated error handling

## Key Features

✅ **Implemented Features**:
1. **Derive Macro**: Automatic `Error` trait implementation with `#[derive(Error)]`
2. **Extension Control**: Explicit `#[extension]` attributes for GraphQL field control
3. **Re-exported Convenience**: `apollo_router_error::Error` provides both trait and derive macro
4. **CamelCase Conversion**: Automatic field name conversion (`error_code` → `"errorCode"`)
5. **Error Registry**: Automatic registration using `linkme` for introspection
6. **GraphQL Compatibility**: Standard GraphQL error format with documented extensions

## Extension Field Control

Only fields with explicit `#[extension]` attributes are included in GraphQL extensions:

```rust
ErrorVariant {
    #[extension("customName")] // ✅ Included as "customName"
    field1: String,
    
    #[extension] // ✅ Included as "field2" (camelCase)
    field_2: u32,
    
    field3: String, // ❌ NOT included in extensions
    
    #[source] // ❌ NOT included (diagnostic field)
    source_error: SomeError,
}
```

This approach provides precise control over what information is exposed in error responses while eliminating type compatibility issues.

## Future Enhancements

1. **Error Analytics**: Collect and analyze error patterns across services
2. **Interactive Documentation**: Error code explorer with live examples
3. **IDE Integration**: Error code lookup and quick fixes in development
4. **Localization**: Multi-language error messages and help text
5. **Enhanced Registry**: Advanced querying and filtering capabilities

This strategy provides a robust foundation for error handling that scales with the project while maintaining excellent developer experience and operational visibility. 