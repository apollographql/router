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
- `dispatch` - Request routing and dispatch coordination
- `http_client` - HTTP client operations
- `bytes_client` - Byte stream client operations (responses contain error streams)
- `json_client` - JSON client operations

**Stream-based Services**: Services marked with "error streams" return streaming responses where each stream item is a `Result<T, E>`. This enables error handling layers to process both successful responses and various error conditions (serialization errors, network errors, etc.) in a consistent manner.

#### Layer Descriptions

**http_server_to_bytes_server**
- **Purpose**: Extracts HTTP request bodies as bytes and transforms back to HTTP responses
- **Input**: `http::Request<Body>` with HTTP Extensions
- **Output**: `BytesRequest` with Router Extensions
- **Reverse Transform**: `BytesResponse` with streams → HTTP response with extracted Extensions
- **Use Case**: Entry point for server-side request processing pipelines

**bytes_server_to_json_server**
- **Purpose**: Parses bytes as JSON with fail-fast error handling
- **Input**: `BytesRequest` with byte streams
- **Output**: `JsonRequest` with parsed JSON body
- **Reverse Transform**: `JsonResponse` with response streams → `BytesResponse` with serialized streams
- **Use Case**: JSON parsing layer after HTTP body extraction

**json_client_to_bytes_client**
- **Purpose**: Serializes JSON client requests to bytes for transmission
- **Input**: `JsonRequest` with JSON body
- **Output**: `BytesRequest` with serialized byte streams
- **Reverse Transform**: `BytesResponse` → `JsonResponse` with deserialized JSON
- **Use Case**: Client-side request serialization before HTTP transmission

**bytes_client_to_http_client**
- **Purpose**: Wraps bytes in HTTP requests for client transmission
- **Input**: `BytesRequest` with byte streams
- **Output**: `http::Request<Body>` with HTTP headers and body
- **Reverse Transform**: HTTP response → `BytesResponse` with extracted body
- **Use Case**: Final client-side transformation before network transmission

**prepare_query**
- **Purpose**: Composite layer orchestrating GraphQL query parsing and planning
- **Input**: `JsonRequest` with GraphQL query, variables, operation name
- **Output**: `ExecutionRequest` with query plan ready for execution
- **Internal Flow**: JSON → QueryParse → QueryPlan → ExecutionRequest
- **Use Case**: Complete GraphQL query preparation before execution

**cache**
- **Purpose**: Intelligent caching with Arc-based storage and Clock-PRO eviction
- **Configuration**: Custom key extraction function and error predicate
- **Caching Strategy**: Successful responses and specific error types based on predicate
- **Performance**: Zero-copy cache hits, minimal overhead for cache misses
- **Use Case**: Performance optimization for repeated requests

**error_to_graphql**
- **Purpose**: Transforms service errors into GraphQL-compliant error responses
- **Input**: Any request type (pass-through)
- **Error Handling**: Converts service errors to GraphQL format with proper extensions
- **Output**: GraphQL error response with null data and formatted errors array
- **Use Case**: Top-level error boundary for GraphQL-compliant error responses

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
// mod.rs - Example layer structure
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
    type Error = BoxError; // All layers use BoxError for compatibility
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    
    fn call(&mut self, req: Request) -> Self::Future {
        // Layer logic here - always follow Extensions pattern:
        // 1. Preserve original Extensions
        // 2. Create cloned Extensions for inner service
        // 3. Return original Extensions in response
        let original_extensions = req.extensions;
        let cloned_extensions = original_extensions.clone();
        
        // Transform request with cloned Extensions
        let inner_req = InnerRequest {
            extensions: cloned_extensions,
            // ... other fields
        };
        
        let future = self.inner.call(inner_req);
        Box::pin(async move {
            let inner_resp = future.await?;
            
            // Transform response back with original Extensions
            Ok(OuterResponse {
                extensions: original_extensions, // Always return original
                // ... other fields from inner_resp
            })
        })
    }
}
```

#### ServiceBuilder Extensions

Layers are exposed through `ServiceBuilderExt` trait with organized methods:

**Server-Side Request Transformations:**
```rust
pub trait ServiceBuilderExt<L> {
    fn http_server_to_bytes_server(self) -> ServiceBuilder<Stack<HttpToBytesLayer, L>>;
    fn bytes_server_to_json_server(self) -> ServiceBuilder<Stack<BytesToJsonLayer, L>>;
    fn prepare_query<P, Pl>(self, parse_service: P, plan_service: Pl) -> ServiceBuilder<Stack<PrepareQueryLayer<P, Pl>, L>>;
}
```

**Client-Side Request Transformations:**
```rust
pub trait ServiceBuilderExt<L> {
    fn json_client_to_bytes_client(self) -> ServiceBuilder<Stack<JsonToBytesLayer, L>>;
    fn bytes_client_to_http_client(self) -> ServiceBuilder<Stack<BytesToHttpLayer, L>>;
}
```

**Utility Layers:**
```rust
pub trait ServiceBuilderExt<L> {
    fn cache<Req, Resp, K, F, P>(self, cache_layer: CacheLayer<Req, Resp, K, F, P>) -> ServiceBuilder<Stack<CacheLayer<Req, Resp, K, F, P>, L>>;
    // Note: error_to_graphql layer is used directly via .layer(ErrorToGraphQLLayer)
}
```

#### Current Layers

**Server-Side Transformation Layers:**
- `http_server_to_bytes_server` - HTTP request to bytes transformation for server processing
- `bytes_server_to_json_server` - Bytes to JSON transformation for server request processing

**Client-Side Transformation Layers:**
- `json_client_to_bytes_client` - JSON to bytes transformation for client requests
- `bytes_client_to_http_client` - Bytes to HTTP transformation for client requests

**Composite Layers:**
- `prepare_query` - Composite layer that orchestrates GraphQL query parsing and planning services

**Utility Layers:**
- `cache` - Intelligent caching layer with Arc-based storage and Clock-PRO eviction
- `error_to_graphql` - Converts service errors to GraphQL-compliant error responses

#### Error Handling Layers

With streaming responses now containing error streams, specialized error handling layers can be implemented:

**Implemented Error Handling Layers:**
- **error_to_graphql** - Transforms service errors into GraphQL-compliant error responses with proper formatting and extensions

**Potential Future Error Handling Layers:**
- **Stream Error Recovery** - Layers that can retry failed stream items or provide fallback responses
- **Error Transformation** - Convert serialization errors into appropriate GraphQL error formats
- **Error Aggregation** - Collect and contextualize errors from streaming operations
- **Error Filtering** - Apply business logic to determine which errors should be exposed vs. handled silently

These layers intercept `Result<T, E>` stream items and can transform errors, implement retry logic, or provide alternative responses before passing the stream to the next layer in the pipeline.

**ErrorToGraphQLLayer Usage:**
```rust
use apollo_router_core::layers::error_to_graphql::ErrorToGraphQLLayer;

let service = ServiceBuilder::new()
    .layer(ErrorToGraphQLLayer)  // Convert errors to GraphQL format
    .http_server_to_bytes_server()
    .bytes_server_to_json_server()
    .service(graphql_service);
```

The `ErrorToGraphQLLayer` should typically be placed at the top of the service stack to catch all errors and ensure they are properly formatted for GraphQL clients.

### 3. Extensions (`src/extensions/`)

The Extensions system provides a type-safe, thread-safe context for storing and retrieving values throughout the request pipeline.

#### Key Features

- **Type-safe**: Values are stored and retrieved by type
- **Thread-safe**: Can be used across multiple threads
- **http::Extensions Compatible**: Built on http::Extensions internally with conversion support
- **Mutable Access Required**: Requires `&mut self` for modifications (no internal mutability)

#### Internal Architecture

Extensions wraps `http::Extensions` directly for simplicity and performance:

```rust
pub struct Extensions {
    inner: http::Extensions,
}
```

- **Simple Wrapper**: Direct wrapper around `http::Extensions`
- **No Hierarchy**: Each Extensions instance is independent
- **Easy Conversion**: Seamless interoperability with `http::Extensions`

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

// Clone Extensions for independent copies
let copy = extensions.clone();

// Convert to/from http::Extensions for interoperability
let http_ext: http::Extensions = extensions.into();
let extensions: Extensions = http_ext.into();
```

#### Conversion Behavior

- **Extensions → http::Extensions**: Extracts the wrapped `http::Extensions` directly
- **http::Extensions → Extensions**: Wraps the `http::Extensions` instance
- **Round-trip Safe**: Converting Extensions to http::Extensions and back preserves all data

#### Extensions in Layers

**Critical Rule**: When implementing layers that transform request types, always use `Extensions::clone()` and return the **original** extensions in the response.

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
            
            // 2. Create cloned extensions for inner service
            let cloned_extensions = original_extensions.clone();
            
            // 3. Transform request with cloned extensions
            let output_req = OutputRequest {
                extensions: cloned_extensions,
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

1. **Context Preservation**: Original request context is preserved throughout the pipeline
2. **Independent Modifications**: Each layer can modify its own copy without affecting the original
3. **Predictable Behavior**: Original extensions are always returned in responses
4. **Flexibility**: Inner services receive all existing context but can add or modify as needed

##### Examples from Existing Layers

**HttpServerToBytesServerLayer**:
```rust
// Extract and preserve original extensions
let original_extensions = parts.extensions.get::<crate::Extensions>().cloned().unwrap_or_default();

// Create cloned extensions for inner service
let cloned_extensions = original_extensions.clone();

let bytes_req = BytesRequest {
    extensions: cloned_extensions, // Inner service gets cloned extensions
    body: body_bytes,
};

// ... call inner service ...

// Return original extensions in HTTP response
http_resp.extensions_mut().insert(original_extensions);
```

**BytesServerToJsonServerLayer**:
```rust
// Preserve original extensions from bytes request
let original_extensions = req.extensions;

// Create cloned extensions for inner service
let cloned_extensions = original_extensions.clone();

let json_req = JsonRequest {
    extensions: cloned_extensions, // Inner service gets cloned extensions
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

1. **Original values are accessible** in the inner service
2. **Original extensions are preserved** in responses
3. **Inner service modifications don't affect** the original context

```rust
#[tokio::test]
async fn test_extensions_passthrough() {
    // Setup original extensions
    let mut extensions = Extensions::default();
    extensions.insert("original_value".to_string());
    extensions.insert(42i32);

    // ... setup layer and mock service ...

    // Verify in mock service:
    // - Original values are accessible
    let original_string: Option<String> = request.extensions.get();
    assert_eq!(original_string, Some("original_value".to_string()));
    
    let original_int: Option<i32> = request.extensions.get();
    assert_eq!(original_int, Some(42));
    
    // Add values to cloned extensions (note: requires &mut access to cloned extensions)
    request.extensions.insert(999i32); // Override existing value
    request.extensions.insert(3.14f64); // Add new type

    // ... call layer ...

    // Verify response preserves original context
    let response_string: Option<String> = response.extensions.get();
    assert_eq!(response_string, Some("original_value".to_string()));
    
    let response_int: Option<i32> = response.extensions.get();
    assert_eq!(response_int, Some(42)); // Original value preserved
    
    let response_float: Option<f64> = response.extensions.get();
    assert_eq!(response_float, None); // Inner additions not visible in response
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
- **Extensions Testing**: Always test that layers properly clone and return original Extensions

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

Services are composed using Tower's `ServiceBuilder` with Apollo Router's extension methods:

**Complete Server-Side Pipeline:**
```rust
use apollo_router_core::layers::ServiceBuilderExt;

// Create the query parsing and planning services
let (query_parse_service, _) = tower_test::mock::spawn();
let (query_plan_service, _) = tower_test::mock::spawn();
let (execution_service, _) = tower_test::mock::spawn();

let server_service = ServiceBuilder::new()
    .layer(error_to_graphql::ErrorToGraphQLLayer) // Error boundary
    .http_server_to_bytes_server()                // HTTP → Bytes
    .bytes_server_to_json_server()               // Bytes → JSON
    .prepare_query(                              // JSON → Execution (composite)
        query_parse_service,
        query_plan_service
    )
    .service(execution_service);
```

**Client-Side Pipeline with Caching:**
```rust
use apollo_router_core::layers::{ServiceBuilderExt, cache::CacheLayer};

let cache_layer = CacheLayer::new(
    1000,                          // Cache capacity
    |req: &JsonRequest| "key".to_string(), // Key extraction function
    |err| false                    // Error predicate (don't cache errors)
);

let (http_client, _) = tower_test::mock::spawn();

let client_service = ServiceBuilder::new()
    .json_client_to_bytes_client()    // JSON → Bytes
    .bytes_client_to_http_client()    // Bytes → HTTP
    .cache(cache_layer)               // Add caching
    .service(http_client);
```

**Mixed Pipeline Example:**
```rust
let service = ServiceBuilder::new()
    .layer(TelemetryLayer)                    // Custom telemetry
    .layer(AuthLayer)                         // Custom authentication
    .cache(cache_layer)                       // Apollo Router caching
    .http_server_to_bytes_server()            // Apollo Router transformation
    .service(CoreService);
```

## Request/Response Flow

Each service in the pipeline transforms requests and responses, with Extensions providing context through simple cloning. Stream-based responses contain error streams that enable error handling layers to process failures:

```
HTTP Request (Original Extensions)
    ↓ http_server_to_bytes_server layer
    ↓ clone() → Cloned Extensions for inner service
Bytes Request (Cloned Extensions)
    ↓ bytes_server_to_json_server layer  
    ↓ clone() → Cloned Extensions for inner service
JSON Request (Cloned Extensions)
    ↓ prepare_query layer (Composite Layer)
    ↓ clone() → Cloned Extensions for inner service
    │   Query Parse Request (Cloned Extensions)
    │       ↓ query_parse service
    │   Query Parse Response 
    │       ↓ query_plan service
    │   Query Plan Response
    │       ↓ combined into ExecutionRequest
Execution Request (Cloned Extensions)
    ↓ (query_execution service) [produces Stream<Result<Item, Error>>]
    ↓ [Error Handling Layers can intercept stream errors]
    ↓ clone() → Cloned Extensions for inner service
Request Dispatcher Request (Cloned Extensions)
    ↓ (request_dispatcher service)
HTTP Response (Returns Original Extensions)
```

**Extensions Flow Rules:**
- Each layer transformation uses `extensions.clone()` 
- Inner services receive cloned Extensions with all existing context
- Response transformations return the **original** Extensions from the request
- Original Extensions are preserved throughout the pipeline
- **Composite Services**: Internal sub-service calls within composite services (like QueryPreparation) use the same cloned Extensions

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
- **Extensions Simplicity**: Each Extensions is independent with no hierarchy overhead
- **Extensions Memory**: Cloning Extensions creates independent copies; use `Arc` for shared expensive data
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