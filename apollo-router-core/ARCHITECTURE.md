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

// Remove values
extensions.remove::<i32>();
```

### 4. JSON (`src/json/`)

Common JSON utilities and type definitions used across services.

## Coding Standards

### Error Handling

- Each service defines its own `Error` enum
- Errors should **never** implement `Clone`
- Use `thiserror::Error` for error definitions
- Errors should be descriptive and actionable

### Testing

- Separate test files: `tests.rs` adjacent to `mod.rs`
- Use `#[cfg(test)] mod tests;` in `mod.rs` files
- Traits should be annotated with `#[cfg_attr(test, mry::mry)]`
- Avoid mocking when possible; prefer `mry` over `mockall` when mocking is necessary
- Use `tower-test` for testing Tower layers and services
- Externalize test fixtures using `include_str!` and prefer YAML format
- Write tests that exercise real implementations, not just mocks

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

Each service in the pipeline transforms requests and responses:

```
HTTP Request
    ↓ (http_server)
Bytes Request
    ↓ (bytes_server)
JSON Request
    ↓ (json_server)
Query Parse Request
    ↓ (query_parse)
Query Plan Request
    ↓ (query_plan)
Execution Request
    ↓ (query_execution)
Fetch Request
    ↓ (fetch)
HTTP Response
```

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
- **Service Composition**: Tower's zero-cost abstractions minimize overhead
- **Async Efficiency**: Native async/await provides optimal performance
- **Memory Management**: Services should be mindful of memory usage patterns

## Future Considerations

- Integration with FastTrace for distributed tracing
- Enhanced monitoring and observability
- Additional service implementations for different protocols
- Configuration system for service composition
- Performance optimization and benchmarking 