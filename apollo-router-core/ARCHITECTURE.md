# Apollo Router Core Architecture

## Overview

Apollo Router Core implements a modular, service-oriented architecture based on **hexagonal architecture principles**. The design separates concerns into distinct layers and provides clear extension points for custom implementations.

## Core Principles

1. **Hexagonal Architecture**: Clear separation between core business logic and external concerns
2. **Service-Oriented**: Each service has a single responsibility and well-defined interfaces
3. **Tower-Based**: Built on the Tower service ecosystem for composability and middleware support
4. **Testable**: Every component can be tested in isolation
5. **Extensible**: Clear extension points for custom implementations

## Directory Structure

```
apollo-router-core/
├── src/
│   ├── lib.rs                 # Main library entry point
│   ├── services/              # Core service implementations
│   │   ├── mod.rs            # Service module exports
│   │   └── {service_name}/   # Individual service directories
│   │       ├── mod.rs        # Service trait, Request/Response, Error
│   │       ├── {impl_name}/  # Implementation subdirectories (if multiple)
│   │       │   ├── mod.rs    # Implementation code
│   │       │   └── tests.rs  # Implementation-specific tests
│   │       └── tests.rs      # Service trait tests (optional)
│   ├── layers/               # Tower layers for cross-cutting concerns
│   │   ├── mod.rs           # Layer exports and extensions
│   │   └── {layer_name}/    # Individual layer directories
│   │       ├── mod.rs       # Layer implementation
│   │       └── tests.rs     # Layer tests
│   ├── extensions/          # Context and extension system
│   │   ├── mod.rs          # Extensions implementation
│   │   └── tests.rs        # Extensions tests
│   └── json/               # JSON utilities and types
│       └── mod.rs          # JSON type definitions
├── Cargo.toml
└── README.md
```

## Architecture Components

### 1. Services (`src/services/`)

Services are the core business logic components. Each service implements a specific transformation or operation in the request/response pipeline.

#### Service Naming Convention

- **Transformation Services**: Named with verbs (e.g., `query_parse`, `query_execution`)
  - Transform data between pipeline stages
  - Have clear input/output types
  
- **Hook Services**: Named without verbs (e.g., `http_server`, `json_server`)
  - Extension points for custom implementations
  - Allow users to inject custom behavior

#### Service Structure

Each service directory contains:

```rust
// mod.rs - Service trait and types
pub struct Request {
    pub extensions: Extensions,
    // ... service-specific fields
}

pub struct Response {
    pub extensions: Extensions,
    // ... service-specific fields
    // Note: Any streams in responses are streams of errors to enable
    // error handling layers and proper serialization error management
}

#[derive(Debug, Error)]
pub enum Error {
    // Service-specific errors
}

#[cfg_attr(test, mry::mry)]
pub trait ServiceName {
    async fn call(&self, req: Request) -> Result<Response, Error>;
}

#[cfg(test)]
mod tests; // Include if trait-level tests exist
```

#### Stream Response Architecture

Services that return streaming responses now use **streams of errors** rather than streams of successful data. This architectural decision enables:

- **Layered Error Handling**: Error handling layers can intercept and transform errors in streams
- **Serialization Error Management**: Instead of defaulting on serialization errors, they flow through the error stream
- **Consistent Error Processing**: All errors, whether immediate or streaming, follow the same handling pipeline
- **Composable Error Transformations**: Multiple layers can participate in error processing and recovery

**Example Stream Response Pattern**:
```rust
pub struct StreamingResponse {
    pub extensions: Extensions,
    // Stream contains Result<SuccessType, ErrorType> instead of just SuccessType
    pub stream: Pin<Box<dyn Stream<Item = Result<ResponseItem, StreamError>> + Send>>,
}
```

This allows error handling layers to:
- Transform serialization errors into GraphQL errors
- Apply retry logic to transient errors
- Add contextual information to streaming errors
- Implement fallback strategies for failed stream items

#### Service Implementation Structure

For services with multiple implementations:

```
services/
├── service_name/
│   ├── mod.rs              # Trait definition
│   ├── default_impl/       # Default implementation
│   │   ├── mod.rs         # Implementation code
│   │   └── tests.rs       # Implementation tests
│   ├── custom_impl/       # Custom implementation
│   │   ├── mod.rs         # Implementation code
│   │   └── tests.rs       # Implementation tests
│   └── tests.rs           # Trait-level tests (if applicable)
```

#### Current Services

- `http_server` - HTTP request handling
- `bytes_server` - Byte stream processing (responses contain error streams)
- `json_server` - JSON request/response handling  
- `query_parse` - GraphQL query parsing
- `query_plan` - Query planning
- `query_preparation` - Composite service combining query parsing and planning
- `query_execution` - Query execution (responses may contain error streams)
- `request_dispatcher` - Request routing and dispatch coordination
- `http_client` - HTTP client operations
- `bytes_client` - Byte stream client operations (responses contain error streams)
- `json_client` - JSON client operations

**Stream-based Services**: Services marked with "error streams" return streaming responses where each stream item is a `Result<T, E>`. This enables error handling layers to process both successful responses and various error conditions (serialization errors, network errors, etc.) in a consistent manner.

#### Composite Services

Some services are **composite services** that internally orchestrate multiple sub-services to provide a higher-level abstraction:

##### QueryPreparation Service

The `query_preparation` service is a composite service that combines `query_parse` and `query_plan` services. It provides a single interface for the complete query preparation phase.

**Input**: JSON request containing a GraphQL query string
**Output**: Execution request containing a query plan ready for execution

**Internal Flow**:
```rust
JSON Request
    ↓ query_parse service
QueryParse Response (Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>>)
    ↓ error conversion & query_plan service  
QueryPlan Response (QueryPlan)
    ↓ transformed to
Execution Request
```

**Error Handling Strategy**:
The query preparation service is responsible for converting executable document validation errors from failed parsing results into appropriate service-level errors. This design allows:

- **Query Parser Focus**: The query parse service focuses purely on parsing GraphQL and returns a clear Result type
- **Centralized Error Conversion**: All error handling and conversion logic is centralized in the composite service
- **Rich Error Context**: Validation errors are preserved with full document context for better error reporting
- **Clean Separation**: Parse success vs. validation failure is clearly represented by Result type
- **Type Safety**: Valid documents are wrapped in `Valid<>` providing additional compile-time guarantees

**Service Architecture**:
```rust
// QueryParseService now takes a schema and returns Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>>
pub struct QueryParseService {
    schema: Valid<Schema>,
}

impl QueryParseService {
    pub fn new(schema: Valid<Schema>) -> Self {
        Self { schema }
    }
    
    // Returns Result to clearly separate success/failure cases
    fn parse_query(&self, query_string: &str) -> Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>> {
        // Direct delegation to apollo_compiler's parse_and_validate
        ExecutableDocument::parse_and_validate(&self.schema, query_string, "query.graphql")
    }
}

// QueryPreparation handles error conversion from WithErrors<ExecutableDocument>
pub struct QueryPreparationService<ParseService, PlanService> {
    parse_service: ParseService,
    plan_service: PlanService,
}

impl<P, Pl> Service<JsonRequest> for QueryPreparationService<P, Pl>
where
    P: Service<QueryParseRequest, Response = QueryParseResponse>,
    Pl: Service<QueryPlanRequest, Response = QueryPlanResponse>,
{
    type Response = ExecutionRequest;
    type Error = QueryPreparationError;
    
    async fn call(&mut self, req: JsonRequest) -> Result<Self::Response, Self::Error> {
        // 1. Transform JSON to QueryParse request
        let parse_req = transform_json_to_parse(req)?;
        
        // 2. Call query_parse service
        let parse_resp = self.parse_service.call(parse_req).await?;
        
        // 3. Handle Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>> - convert validation errors
        let executable_doc = match parse_resp.query {
            Ok(valid_doc) => valid_doc.into_inner(), // Extract document from Valid wrapper
            Err(with_errors) => {
                // Convert apollo_compiler validation errors to service errors
                return Err(Self::convert_validation_errors(with_errors.errors));
            }
        };
        
        // 4. Transform to QueryPlan request with validated document
        let plan_req = transform_parse_to_plan_with_doc(executable_doc, parse_resp)?;
        
        // 5. Call query_plan service
        let plan_resp = self.plan_service.call(plan_req).await?;
        
        // 6. Transform QueryPlan response to Execution request
        Ok(transform_plan_to_execution(plan_resp)?)
    }
}

This pattern allows:
- **Simplified Integration**: Consumers only need to interact with one service
- **Internal Optimization**: The composite service can optimize the transition between sub-services
- **Testing Flexibility**: Sub-services can be tested independently or as a composite
- **Service Reuse**: Individual sub-services can still be used independently when needed

### 2. Layers (`src/layers/`)

Layers implement cross-cutting concerns using Tower's layer system. They provide middleware functionality that can be composed into service stacks.

#### Layer Structure

```rust
// mod.rs
pub struct LayerName;

impl<S> Layer<S> for LayerName {
    type Service = LayerNameService<S>;
    
    fn layer(&self, inner: S) -> Self::Service {
        LayerNameService { inner }
    }
}

pub struct LayerNameService<S> {
    inner: S,
}

impl<S> Service<Request> for LayerNameService<S>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;
    
    fn call(&mut self, req: Request) -> Self::Future {
        // Layer logic here
        self.inner.call(req)
    }
}
```

#### ServiceBuilder Extensions

Layers are exposed through `ServiceBuilderExt` trait:

```rust
pub trait ServiceBuilderExt<L> {
    fn layer_name(self) -> ServiceBuilder<Stack<LayerNameLayer, L>>;
}
```

#### Current Layers

- `http_to_bytes` - HTTP to bytes transformation
- `bytes_to_json` - Bytes to JSON transformation

#### Error Handling Layers

With streaming responses now containing error streams, specialized error handling layers can be implemented:

- **Stream Error Recovery** - Layers that can retry failed stream items or provide fallback responses
- **Error Transformation** - Convert serialization errors into appropriate GraphQL error formats
- **Error Aggregation** - Collect and contextualize errors from streaming operations
- **Error Filtering** - Apply business logic to determine which errors should be exposed vs. handled silently

These layers intercept `Result<T, E>` stream items and can transform errors, implement retry logic, or provide alternative responses before passing the stream to the next layer in the pipeline.

### 3. Extensions (`src/extensions/`)

The Extensions system provides a type-safe, thread-safe context for storing and retrieving values throughout the request pipeline.

#### Key Features

- **Type-safe**: Values are stored and retrieved by type
- **Thread-safe**: Can be used across multiple threads
- **Clone-efficient**: Designed to be cloned cheaply using Arc for parent chains
- **http::Extensions Compatible**: Built on http::Extensions internally with conversion support
- **Mutable Access Required**: Requires `&mut self` for modifications (no internal mutability)

#### Internal Architecture

Extensions uses an enum-based internal architecture for optimal flexibility:

```rust
enum ExtensionsInner {
    /// Native http::Extensions storage for new Extensions
    Native(http::Extensions),
    /// Wrapped http::Extensions when converted from external sources
    HttpWrapped(http::Extensions),
}
```

- **Native**: Created via `Extensions::new()` or `extend()`
- **HttpWrapped**: Created when converting from external `http::Extensions`
- **Parent Chain**: Uses `Arc<Extensions>` for efficient hierarchical sharing
- **Conversions**: Direct extraction/wrapping without intermediate wrapper types

#### Usage Pattern

```rust
use apollo_router_core::Extensions;

let mut extensions = Extensions::new();

// Store values (requires &mut self)
extensions.insert(42i32);
extensions.insert("hello".to_string());

// Retrieve values
let number: Option<i32> = extensions.get();
let text: Option<String> = extensions.get();

// Convert to/from http::Extensions for interoperability
let http_ext: http::Extensions = extensions.into();
let extensions: Extensions = http_ext.into();
```

#### Conversion Behavior

- **Extensions → http::Extensions**: Extracts the current layer's `http::Extensions` directly
- **http::Extensions → Extensions**: Creates an `HttpWrapped` variant
- **Hierarchical Data**: Only current layer data is included in conversions, not parent layers
- **Round-trip Safe**: Converting Extensions to http::Extensions and back preserves the data

#### Hierarchical Extensions System

Extensions supports a hierarchical architecture through the `extend()` method:

```rust
let mut parent = Extensions::default();
parent.insert("upstream_value".to_string());

let mut child = parent.extend();
child.insert(42i32); // New type, allowed
child.insert("downstream_attempt".to_string()); // Same type as parent

// Parent values always take precedence
assert_eq!(child.get::<String>(), Some("upstream_value".to_string()));
assert_eq!(child.get::<i32>(), Some(42));

// Parent only sees its own values
assert_eq!(parent.get::<i32>(), None);
```

#### Extensions in Layers

**Critical Rule**: When implementing layers that transform request types, always use `Extensions::extend()` and return the **original** extensions in the response.

##### Correct Layer Implementation Pattern

```rust
impl<S> Service<InputRequest> for LayerService<S>
where
    S: Service<OutputRequest>,
{
    fn call(&mut self, req: InputRequest) -> Self::Future {
        Box::pin(async move {
            // 1. Preserve original extensions
            let original_extensions = req.extensions;
            
            // 2. Create extended layer for inner service
            let extended_extensions = original_extensions.extend();
            
            // 3. Transform request with extended extensions
            let output_req = OutputRequest {
                extensions: extended_extensions,
                // ... other transformed fields
            };
            
            // 4. Call inner service
            let output_resp = inner.call(output_req).await?;
            
            // 5. Transform response back with ORIGINAL extensions
            let input_resp = InputResponse {
                extensions: original_extensions, // ✅ Always return original
                // ... other transformed fields from output_resp
            };
            
            Ok(input_resp)
        })
    }
}
```

##### Why This Pattern Matters

1. **Upstream Precedence**: Parent layers' decisions cannot be overridden by downstream services
2. **Context Isolation**: Each layer can add context without affecting parent layers
3. **Predictable Behavior**: Original request context is preserved throughout the pipeline
4. **Hierarchical Inheritance**: Inner services can access parent context while adding their own

##### Examples from Existing Layers

**HttpToBytesLayer**:
```rust
// Extract and preserve original extensions
let original_extensions = parts.extensions.get::<crate::Extensions>().cloned().unwrap_or_default();

// Create extended layer for inner service
let extended_extensions = original_extensions.extend();

let bytes_req = BytesRequest {
    extensions: extended_extensions, // Inner service gets extended layer
    body: body_bytes,
};

// ... call inner service ...

// Return original extensions in HTTP response
http_resp.extensions_mut().insert(original_extensions);
```

**BytesToJsonLayer**:
```rust
// Preserve original extensions from bytes request
let original_extensions = req.extensions;

// Create extended layer for inner service
let extended_extensions = original_extensions.extend();

let json_req = JsonRequest {
    extensions: extended_extensions, // Inner service gets extended layer
    body: json_body,
};

// ... call inner service ...

// Return original extensions in bytes response
let bytes_resp = BytesResponse {
    extensions: original_extensions, // Always return original
    responses: Box::pin(bytes_stream),
};
```

#### Testing Extensions in Layers

When testing layer Extensions handling, verify:

1. **Parent values are accessible** in the inner service
2. **Original extensions are preserved** in responses
3. **Inner service modifications are isolated** from original context

```rust
#[tokio::test]
async fn test_extensions_passthrough() {
    // Setup original extensions
    let mut extensions = Extensions::default();
    extensions.insert("original_value".to_string());
    extensions.insert(42i32);

    // ... setup layer and mock service ...

    // Verify in mock service:
    // - Parent values are accessible
    let parent_string: Option<String> = request.extensions.get();
    assert_eq!(parent_string, Some("original_value".to_string()));
    
    let parent_int: Option<i32> = request.extensions.get();
    assert_eq!(parent_int, Some(42));
    
    // Add values to extended layer (note: requires &mut access to extended layer)
    request.extensions.insert(999i32); // Try to override
    request.extensions.insert(3.14f64); // Add new type

    // ... call layer ...

    // Verify response preserves original context
    let response_string: Option<String> = response.extensions.get();
    assert_eq!(response_string, Some("original_value".to_string()));
    
    let response_int: Option<i32> = response.extensions.get();
    assert_eq!(response_int, Some(42)); // Original value, not 999
    
    let response_float: Option<f64> = response.extensions.get();
    assert_eq!(response_float, None); // Inner additions not visible
}
```

### 4. JSON (`src/json/`)

Common JSON utilities and type definitions used across services.

## Coding Standards

### Error Handling

Apollo Router Core uses a comprehensive error handling strategy with automatic derive macros:

#### Error Definition Pattern

Each service defines its own error enum using the re-exported derive macro:

```rust
use apollo_router_error::Error; // Gets both trait and derive macro

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum MyServiceError {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR),
        help("Check your configuration file")
    )]
    ConfigError {
        #[extension("configMessage")] // Explicit GraphQL extension field
        message: String,
        #[extension] // Will be camelCase: "errorCode"
        error_code: u32,
        #[source_code] // NOT included in GraphQL extensions
        config_source: Option<String>,
    },
}
```

#### Error Handling Principles

- **Derive Macro**: Use `#[derive(Error)]` for automatic trait implementation
- **Explicit Extensions**: Only fields with `#[extension]` attributes appear in GraphQL errors
- **No Clone**: Errors should **never** implement `Clone`
- **No Downstream**: Errors should **never** have a `Downstream` variant
- **Rich Diagnostics**: Use `thiserror::Error` and `miette::Diagnostic` for comprehensive error information
- **Structured Codes**: Follow hierarchical error code pattern using screaming snake case (`APOLLO_ROUTER_COMPONENT_CATEGORY_ERROR`)

#### GraphQL Extension Control

The new error system provides precise control over GraphQL error extensions:

```rust
ErrorVariant {
    #[extension("customName")] // ✅ Included as "customName"
    field1: String,
    
    #[extension] // ✅ Included as "camelCaseField"
    camel_case_field: String,
    
    regular_field: String, // ❌ NOT included in extensions
    
    #[source] // ❌ NOT included (diagnostic field)
    source_error: SomeError,
}
```

This approach provides explicit control over error data exposure and ensures type safety for GraphQL extensions.

### BoxError for Service Error Types

**Critical Principle**: All services and layers **must** use `tower::BoxError` for their error types to ensure downstream errors can be passed through unwrapped.

#### Service Error Type Pattern

Services should follow this error type pattern:

```rust
use apollo_router_error::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum ServiceError {
    /// Service-specific error variant
    #[error("Specific error description: {message}")]
    #[diagnostic(code(APOLLO_ROUTER_SERVICE_SPECIFIC_ERROR))]
    SpecificError {
        #[extension("errorMessage")]
        message: String,
    },
    
    /// Another service-specific error variant  
    #[error("Another error occurred")]
    #[diagnostic(code(APOLLO_ROUTER_SERVICE_ANOTHER_ERROR))]
    AnotherError {
        #[source]
        cause: SomeSpecificError,
        #[extension("errorContext")]
        context: String,
    },
}
```

#### Layer Error Type Pattern

Layers should follow this error type pattern:

```rust
use apollo_router_error::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum LayerError {
    /// Layer-specific error variants
    #[error("Layer operation failed: {operation}")]
    #[diagnostic(code(APOLLO_ROUTER_LAYERS_OPERATION_FAILED))]
    LayerSpecificError {
        #[extension("failedOperation")]
        operation: String,
        #[source]
        cause: SomeLayerError,
    },
    
    /// Other layer-specific error variants as needed
    #[error("Configuration error: {reason}")]
    #[diagnostic(code(APOLLO_ROUTER_LAYERS_CONFIGURATION_ERROR))]
    ConfigurationError {
        #[extension("configReason")]
        reason: String,
    },
}
```

#### Benefits of This Pattern

1. **Error Transparency**: Downstream errors flow through the service stack without being wrapped in generic variants
2. **Type Safety**: Each service can define specific error variants for its operations without pollution from downstream concerns
3. **Debugging**: Error chains preserve the original error context without unnecessary wrapping layers
4. **Clean Separation**: Services only define errors for their own failure modes, not for propagating downstream failures
5. **Interoperability**: All services work together seamlessly in Tower service stacks through BoxError conversion

#### Service Implementation Guidelines

When implementing services:

```rust
impl<S> Service<Request> for MyService<S>
where
    S: Service<DownstreamRequest, Response = DownstreamResponse>,
    S::Error: Into<tower::BoxError>, // Always require BoxError conversion
{
    type Response = MyResponse;
    type Error = MyServiceError; // Your specific error enum
    
    fn call(&mut self, req: Request) -> Self::Future {
        // ... service logic ...
        
        // Let downstream errors bubble up as BoxError
        let result = self.inner.call(downstream_req)
            .await
            .map_err(Into::into)?; // Convert to BoxError directly
            
        // ... continue processing ...
    }
}
```

This pattern ensures that error information flows correctly through the service pipeline while maintaining type safety and clear error boundaries.

### Testing

- Separate test files: `tests.rs` adjacent to `mod.rs`
- Use `#[cfg(test)] mod tests;` in `mod.rs` files
- Traits should be annotated with `#[cfg_attr(test, mry::mry)]`
- **Use TowerTest consistently**: Always use `TowerTest` from `test_utils::tower_test` instead of creating custom mock services
- Avoid mocking when possible; prefer `mry` over `mockall` when mocking is necessary
- Externalize test fixtures using `include_str!` and prefer YAML format
- Write tests that exercise real implementations, not just mocks
- **Extensions Testing**: Always test that layers properly extend and return original Extensions

#### TowerTest Guidelines

**Critical Principle**: All Tower service and layer tests **must** use the `TowerTest` utility instead of creating custom `MockService` implementations.

##### Why TowerTest Over Custom Mocks

- **Standardized Testing**: Consistent test patterns across the codebase
- **Automatic Timeout Protection**: Prevents hanging tests with configurable timeouts
- **Panic Detection**: Catches and reports panics in test expectations clearly
- **Type Inference**: No type annotations required for test expectations
- **Better Error Messages**: Clear failure messages when tests fail or timeout
- **Maintenance**: Centralized test utilities reduce code duplication

##### TowerTest Usage Pattern

Always use this pattern for layer and service testing:

```rust
use crate::test_utils::tower_test::TowerTest;

#[tokio::test]
async fn test_my_layer() -> Result<(), Box<dyn std::error::Error>> {
    let layer = MyLayer::new();
    let request = MyRequest::new("test");

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(request, |mut downstream| async move {
            downstream.allow(1);
            let (req, resp) = downstream.next_request().await.expect("should receive request");
            // Verify and respond to the request
            resp.send_response(MyResponse::success());
        })
        .await?;

    // Verify final response
    assert_eq!(response.status, "success");
    Ok(())
}
```

##### Migration from Custom Mocks

When encountering tests with custom `MockService` implementations:

1. **Remove** the custom mock service struct and implementation
2. **Replace** with `TowerTest::builder().layer().oneshot()` pattern
3. **Update** test logic to use expectations closure for downstream behavior
4. **Add** proper error handling with `Result<(), Box<dyn std::error::Error>>` return type

**Before (❌ Avoid)**:
```rust
struct MockService { /* custom implementation */ }
let mock = MockService::new(responses);
let service = layer.layer(mock);
let response = service.call(request).await;
```

**After (✅ Correct)**:
```rust
let response = TowerTest::builder()
    .layer(layer)
    .oneshot(request, |mut downstream| async move {
        downstream.allow(1);
        let (req, resp) = downstream.next_request().await.expect("should receive request");
        resp.send_response(expected_response);
    })
    .await?;
```

#### Error Testing

Use the `assert_error!` macro for type-safe error testing:

```rust
use crate::assert_error;

// Test specific error variant with pattern matching
assert_error!(result, MyLayerError, MyLayerError::SpecificVariant { .. });

// Test error type only (any variant)
assert_error!(result, MyLayerError);
```

The `assert_error!` macro provides:
- **Type-safe downcasting** from `BoxError` to specific error types
- **Pattern matching** on error variants with compile-time verification
- **Clear failure messages** when assertions fail
- **Concise syntax** replacing verbose downcasting boilerplate

This approach is preferred over testing error messages or error codes as it catches changes at compile time and is more maintainable.

#### Testing Tower Services and Layers

For testing Tower services and layers, use the `TowerTest` builder utility from `test_utils::tower_test`. This provides a fluent API with automatic timeout protection, panic detection, and clean separation of test configuration.

##### Key Features

- **Automatic timeout protection**: Prevents hanging tests (default: 1 second)
- **Panic detection**: Catches panics in expectation handlers and provides clear error messages
- **Type inference**: No type annotations required for expectations
- **Fluent API**: Clean, readable test configuration
- **Flexible testing**: Supports both oneshot and custom test scenarios

##### Basic Layer Testing Pattern

```rust
use crate::test_utils::tower_test::TowerTest;
use std::time::Duration;

#[tokio::test]
async fn test_my_layer() -> Result<(), Box<dyn std::error::Error>> {
    let layer = MyLayer::new();
    let request = MyRequest::new("test");

    let response = TowerTest::builder()
        .timeout(Duration::from_secs(2)) // Optional: override default timeout
        .layer(layer)
        .oneshot(request, |mut downstream| async move {
            // Set up downstream service expectations
            downstream.allow(1); // Allow one request
            
            let (received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");
            
            // Verify the transformed request
            assert_eq!(received_req.some_field, "expected_value");
            
            // Send mock response back
            send_response.send_response(MyDownstreamResponse::success());
        })
        .await?;

    // Verify the final response
    assert_eq!(response.status, "success");
    Ok(())
}
```

##### Custom Test Scenarios

For more complex testing scenarios, use the `test()` method:

```rust
#[tokio::test] 
async fn test_multiple_requests() -> Result<(), Box<dyn std::error::Error>> {
    let layer = MyLayer::new();

    let result = TowerTest::builder()
        .layer(layer)
        .test(
            |mut service| async move {
                // Custom test logic with multiple service calls
                let response1 = service.call(MyRequest::new("first")).await?;
                let response2 = service.call(MyRequest::new("second")).await?;
                
                Ok((response1, response2))
            },
            |mut downstream| async move {
                // Handle multiple downstream expectations
                downstream.allow(2);
                
                for expected in ["transformed_first", "transformed_second"] {
                    let (req, resp) = downstream.next_request().await.unwrap();
                    assert_eq!(req.data, expected);
                    resp.send_response(MyDownstreamResponse::ok());
                }
            }
        )
        .await?;

    // Verify both responses
    assert!(result.0.is_success());
    assert!(result.1.is_success());
    Ok(())
}
```

##### Testing Service Implementations

When testing service implementations (not layers), use `tower_test::mock::spawn` for terminal services:

```rust
use tower_test::mock;
use tower::ServiceExt;

#[tokio::test]
async fn test_terminal_service() {
    let service = MyTerminalService::new();
    let mut mock = mock::spawn();
    
    // No downstream service needed for terminal services
    let response = service.oneshot(MyRequest::new()).await.unwrap();
    
    assert_eq!(response.result, "expected");
}
```

##### Testing Guidelines for Tower Components

1. **Layer Tests**: Use `TowerTest::builder().layer()` for middleware layers
2. **Terminal Service Tests**: Use `tower_test::mock::spawn()` for services without downstream dependencies  
3. **Timeout Configuration**: Adjust timeouts for slow operations, but keep them reasonable
4. **Expectation Clarity**: Set clear expectations in the closure - the test will fail if they're not met
5. **Error Propagation**: Use the `?` operator to propagate errors from test operations
6. **Panic Safety**: The test utility catches panics in expectations and converts them to test failures

##### Common Testing Patterns

**Testing Request Transformation**:
```rust
// Verify layer transforms request correctly
let (received_req, _) = downstream.next_request().await.unwrap();
assert_eq!(received_req.transformed_field, expected_value);
```

**Testing Response Transformation**:
```rust  
// Send specific response and verify transformation
send_response.send_response(DownstreamResponse::with_data("test"));
// Then verify the final response outside the expectations closure
assert_eq!(response.processed_data, "processed_test");
```

**Testing Error Handling**:
```rust
send_response.send_error(SomeError::new("test error"));
// Verify error is properly transformed or propagated
```

##### Timeout and Deadlock Prevention

The test utility automatically applies timeouts to prevent hanging tests. Common causes of timeouts:

- Not calling `downstream.allow(n)` with the correct number of expected requests
- Not calling `next_request()` for each allowed request
- Deadlocks in service logic
- Infinite loops or blocking operations

If tests timeout, check that expectations match actual service behavior and that all async operations are properly awaited.

### Builder Pattern

- Use the `bon` library for builders
- Implement builders for complex request/response structures

### Async/Traits

- Use native `async fn` in traits (do not use `async_trait`)
- Prefer `async fn` over returning `Future` types

### Type Safety

- Traits should not expose client types directly
- Create new types and implement `From` for conversions
- Avoid exposing nested traits in return types (exception: returning `Self`)
- Never declare top-level lifetimes in traits, only on individual functions

### Service Composition

Services are composed using Tower's `ServiceBuilder`:

```rust
let service = ServiceBuilder::new()
    .layer(TelemetryLayer)
    .layer(AuthLayer)
    .layer(CacheLayer)
    .service(CoreService);
```

## Request/Response Flow

Each service in the pipeline transforms requests and responses, with Extensions providing hierarchical context. Stream-based responses contain error streams that enable error handling layers to process failures:

```
HTTP Request (Extensions Layer 0)
    ↓ http_to_bytes layer
    ↓ extends() → Extensions Layer 1
Bytes Request (Extensions Layer 1)
    ↓ bytes_to_json layer  
    ↓ extends() → Extensions Layer 2
JSON Request (Extensions Layer 2)
    ↓ (json_server)
    ↓ extends() → Extensions Layer 3
Query Preparation Request (Extensions Layer 3)
    ↓ (query_preparation) - Composite Service:
    │   JSON Request 
    │       ↓ query_parse
    │   QueryParse Response 
    │       ↓ query_plan
    │   QueryPlan Response
    ↓ extends() → Extensions Layer 4
Execution Request (Extensions Layer 4)
    ↓ (query_execution) [produces Stream<Result<Item, Error>>]
    ↓ [Error Handling Layers can intercept stream errors]
    ↓ extends() → Extensions Layer 5
Request Dispatcher Request (Extensions Layer 5)
    ↓ (request_dispatcher)
HTTP Response (Returns Layer 0 - Original Extensions)
```

**Extensions Flow Rules:**
- Each layer transformation uses `extensions.extend()` 
- Inner services receive extended Extensions with access to parent context
- Response transformations return the **original** Extensions from the request
- Parent values always take precedence over child values
- **Composite Services**: Internal sub-service calls within composite services (like QueryPreparation) maintain the same Extensions layer - they don't create additional extension layers

**Error Stream Flow:**
- Streaming services return `Stream<Result<T, E>>` instead of `Stream<T>`
- Error handling layers can intercept, transform, or recover from errors in streams
- Serialization errors flow through the error stream rather than causing default responses
- Multiple error handling layers can be composed to provide comprehensive error management
- Error context is preserved through the Extensions system during error processing

## Extension Points

The architecture provides several extension points:

1. **Custom Service Implementations**: Implement service traits with custom logic
2. **Custom Layers**: Add middleware through Tower layers
3. **Custom Extensions**: Store custom data in the Extensions context
4. **Service Composition**: Compose services using `ServiceBuilder`

## Development Workflow

1. **Define Service Interface**: Create trait, Request/Response types, and Error enum
2. **Implement Service**: Create implementation in subdirectory with tests
3. **Add Layers**: Implement any required middleware layers
4. **Compose Pipeline**: Use `ServiceBuilder` to compose the service stack
5. **Test Integration**: Write integration tests that exercise the full pipeline

## Performance Considerations

- **Extensions Cloning**: Values are cloned when retrieved; wrap expensive types in `Arc`
- **Extensions Hierarchy**: The `extend()` method creates lightweight Arc references to parent Extensions
- **Extensions Memory**: Each layer adds minimal overhead; parent layers are shared via Arc, not copied
- **http::Extensions Compatibility**: Built on standard http::Extensions for maximum ecosystem compatibility
- **Service Composition**: Tower's zero-cost abstractions minimize overhead
- **Async Efficiency**: Native async/await provides optimal performance
- **Memory Management**: Services should be mindful of memory usage patterns
- **Error Stream Processing**: Error streams add minimal overhead to successful paths while enabling comprehensive error handling
- **Stream Backpressure**: Error handling layers should respect stream backpressure to avoid memory buildup
- **Error Recovery Costs**: Error handling layers should be designed to minimize performance impact on successful operations

## Future Considerations

- Integration with FastTrace for distributed tracing
- Enhanced monitoring and observability
- Additional service implementations for different protocols
- Configuration system for service composition
- Performance optimization and benchmarking
- **Advanced Error Handling**: Sophisticated error recovery strategies using the error stream architecture
- **Stream Error Analytics**: Monitoring and metrics collection for streaming error patterns
- **Adaptive Error Policies**: Dynamic error handling behavior based on runtime conditions
- **Error Circuit Breaking**: Circuit breaker patterns for streaming services with high error rates 