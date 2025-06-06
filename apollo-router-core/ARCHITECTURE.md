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
- `bytes_server` - Byte stream processing
- `json_server` - JSON request/response handling
- `query_parse` - GraphQL query parsing
- `query_plan` - Query planning
- `query_execution` - Query execution
- `fetch` - Data fetching coordination
- `http_client` - HTTP client operations
- `bytes_client` - Byte stream client operations
- `json_client` - JSON client operations

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

### 3. Extensions (`src/extensions/`)

The Extensions system provides a type-safe, thread-safe context for storing and retrieving values throughout the request pipeline.

#### Key Features

- **Type-safe**: Values are stored and retrieved by type
- **Thread-safe**: Can be used across multiple threads
- **Clone-efficient**: Designed to be cloned cheaply
- **Capacity-managed**: Uses LRU caching internally

#### Usage Pattern

```rust
use apollo_router_core::Extensions;

let extensions = Extensions::new(1000);

// Store values
extensions.insert(42i32);
extensions.insert("hello".to_string());

// Retrieve values
let number: Option<i32> = extensions.get();
let text: Option<String> = extensions.get();
```

#### Hierarchical Extensions System

Extensions supports a hierarchical architecture through the `extend()` method:

```rust
let parent = Extensions::default();
parent.insert("upstream_value".to_string());

let child = parent.extend();
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
    
    // Add values to extended layer
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

- Each service defines its own `Error` enum
- Errors should **never** implement `Clone`
- Use `thiserror::Error` for error definitions
- Errors should be descriptive and actionable

### BoxError for Service Error Types

**Critical Principle**: All services and layers **must** use `tower::BoxError` for their error types to ensure downstream errors can be passed through unwrapped.

#### Service Error Type Pattern

Services should follow this error type pattern:

```rust
#[derive(Debug, Error)]
pub enum ServiceError {
    /// Service-specific error variant
    #[error("Specific error description: {0}")]
    SpecificError(String),
    
    /// Another service-specific error variant  
    #[error("Another error: {0}")]
    AnotherError(#[from] SomeSpecificError),
    
    /// Downstream service error (always present)
    #[error("Downstream service error: {0}")]
    Downstream(#[from] tower::BoxError),
}
```

#### Layer Error Type Pattern

Layers should follow this error type pattern:

```rust
#[derive(Debug, Error)]
pub enum LayerError {
    /// Layer-specific error variants
    #[error("Layer operation failed: {0}")]
    LayerSpecificError(#[from] SomeLayerError),
    
    /// Downstream service error (always present)
    #[error("Downstream service error: {0}")]
    Downstream(#[from] tower::BoxError),
}
```

#### Benefits of This Pattern

1. **Error Transparency**: Downstream errors flow through the service stack without modification
2. **Type Safety**: Each service can define specific error variants for its operations
3. **Debugging**: Error chains preserve the original error context
4. **Flexibility**: Services can handle specific error types while allowing others to pass through
5. **Interoperability**: All services work together seamlessly in Tower service stacks

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
        
        // Convert downstream errors using the Downstream variant
        let result = self.inner.call(downstream_req)
            .await
            .map_err(|e| MyServiceError::Downstream(e.into()))?;
            
        // ... continue processing ...
    }
}
```

This pattern ensures that error information flows correctly through the service pipeline while maintaining type safety and clear error boundaries.

### Testing

- Separate test files: `tests.rs` adjacent to `mod.rs`
- Use `#[cfg(test)] mod tests;` in `mod.rs` files
- Traits should be annotated with `#[cfg_attr(test, mry::mry)]`
- Avoid mocking when possible; prefer `mry` over `mockall` when mocking is necessary
- Use `tower-test` for testing Tower layers and services
- Externalize test fixtures using `include_str!` and prefer YAML format
- Write tests that exercise real implementations, not just mocks
- **Extensions Testing**: Always test that layers properly extend and return original Extensions

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

Each service in the pipeline transforms requests and responses, with Extensions providing hierarchical context:

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
Query Parse Request (Extensions Layer 3)
    ↓ (query_parse)
    ↓ extends() → Extensions Layer 4
Query Plan Request (Extensions Layer 4)
    ↓ (query_plan)
    ↓ extends() → Extensions Layer 5
Execution Request (Extensions Layer 5)
    ↓ (query_execution)
    ↓ extends() → Extensions Layer 6
Fetch Request (Extensions Layer 6)
    ↓ (fetch)
HTTP Response (Returns Layer 0 - Original Extensions)
```

**Extensions Flow Rules:**
- Each layer transformation uses `extensions.extend()` 
- Inner services receive extended Extensions with access to parent context
- Response transformations return the **original** Extensions from the request
- Parent values always take precedence over child values

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
- **Extensions Hierarchy**: The `extend()` method creates lightweight references to parent layers
- **Extensions Memory**: Each layer adds minimal overhead; parent layers are referenced, not copied
- **Service Composition**: Tower's zero-cost abstractions minimize overhead
- **Async Efficiency**: Native async/await provides optimal performance
- **Memory Management**: Services should be mindful of memory usage patterns

## Future Considerations

- Integration with FastTrace for distributed tracing
- Enhanced monitoring and observability
- Additional service implementations for different protocols
- Configuration system for service composition
- Performance optimization and benchmarking 