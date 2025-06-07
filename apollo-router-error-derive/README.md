# Apollo Router Error Derive Macro

This crate provides a derive macro that automatically implements the `RouterError` trait and registers error types with the global error registry using `linkme`.

## Features

✅ **Automatic RouterError Implementation**: Generates the `error_code()` and `populate_graphql_extensions()` methods from `#[diagnostic]` attributes

✅ **Automatic Error Registration**: Uses `linkme` to automatically register errors in a global registry for introspection

✅ **GraphQL Extensions Generation**: Automatically populates GraphQL error extensions with structured error data

✅ **Error Code Extraction**: Extracts error codes from `#[diagnostic(code(...))]` attributes

✅ **Component/Category Classification**: Automatically extracts component and category from hierarchical error codes

## Quick Start

Add the derive macro to your error enum:

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

    #[error("Parsing failed: {reason}")]
    #[diagnostic(
        code(apollo_router::my_service::parse_error),
        help("Ensure the input is properly formatted")
    )]
    ParseError {
        reason: String,
        line: u32,
        column: u32,
    },
}
```

## What the Derive Macro Generates

The macro automatically generates:

### 1. RouterError Trait Implementation

```rust
impl RouterError for MyServiceError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::ConfigError { .. } => "apollo_router::my_service::config_error",
            Self::NetworkError(_) => "apollo_router::my_service::network_error",
            Self::ParseError { .. } => "apollo_router::my_service::parse_error",
        }
    }
    
    fn populate_graphql_extensions(&self, details: &mut HashMap<String, serde_json::Value>) {
        match self {
            Self::ConfigError { message, .. } => {
                details.insert("errorType".to_string(), "config".into());
                details.insert("message".to_string(), message.clone().into());
            },
            Self::NetworkError(_) => {
                details.insert("errorType".to_string(), "network".into());
            },
            Self::ParseError { reason, line, column } => {
                details.insert("errorType".to_string(), "syntax".into());
                details.insert("reason".to_string(), reason.clone().into());
                details.insert("line".to_string(), (*line).into());
                details.insert("column".to_string(), (*column).into());
            },
        }
    }
}
```

### 2. Automatic Registry Entry

```rust
apollo_router_error::register_error! {
    type_name: "MyServiceError",
    error_code: "apollo_router::my_service::config_error",
    category: "my_service",
    component: "config_error",
    variants: [
        apollo_router_error::ErrorVariantInfo {
            name: "ConfigError",
            code: "apollo_router::my_service::config_error",
            help: Some("Check your configuration parameters"),
            graphql_fields: vec!["message"],
        },
        apollo_router_error::ErrorVariantInfo {
            name: "NetworkError", 
            code: "apollo_router::my_service::network_error",
            help: None,
            graphql_fields: vec![],
        },
        apollo_router_error::ErrorVariantInfo {
            name: "ParseError",
            code: "apollo_router::my_service::parse_error", 
            help: Some("Ensure the input is properly formatted"),
            graphql_fields: vec!["reason", "line", "column"],
        },
    ]
}
```

## Error Code Conventions

Error codes must follow the hierarchical format:

```
apollo_router::{component}::{category}::{specific_error}
```

Examples:
- `apollo_router::query_parse::syntax_error`
- `apollo_router::layers::bytes_to_json::conversion_error`
- `apollo_router::http_server::config_error`
- `apollo_router::execution::timeout`

## Field Handling

The derive macro automatically handles different field types:

### Regular Fields
All regular fields are included in GraphQL extensions:

```rust
#[derive(Error, Diagnostic, Debug, RouterError)]
pub enum MyError {
    #[diagnostic(code(apollo_router::service::error))]
    MyVariant {
        message: String,    // → GraphQL extensions
        code: u32,         // → GraphQL extensions
        timestamp: String, // → GraphQL extensions
    },
}
```

### Special Fields
Certain fields receive special handling:

```rust
#[derive(Error, Diagnostic, Debug, RouterError)]  
pub enum MyError {
    #[diagnostic(code(apollo_router::service::error))]
    MyVariant {
        message: String,
        #[source]              // ← Excluded from GraphQL extensions
        underlying: SomeError,
        #[source_code]         // ← Excluded from GraphQL extensions  
        source_text: Option<String>,
        #[label("Error here")]
        span: Option<SourceSpan>,
    },
}
```

### From Fields
Fields with `#[from]` are handled automatically:

```rust
#[derive(Error, Diagnostic, Debug, RouterError)]
pub enum MyError {
    #[diagnostic(code(apollo_router::service::json_error))]
    JsonError(#[from] serde_json::Error), // Automatically handled
}
```

## GraphQL Error Type Inference

The macro automatically infers GraphQL error types based on error code patterns:

- `*::syntax_error` or `*::parse*` → `"syntax"`
- `*::config*` → `"config"`
- `*::timeout*` → `"timeout"`
- `*::network*` → `"network"`
- `*::conversion*` → `"conversion"`
- `*::json*` → `"json"`
- Others → `"generic"`

## Error Registry Introspection

Once your errors are registered, you can introspect them at runtime:

```rust
use apollo_router_error::{get_registered_errors, get_error_stats};

// Get all registered errors
let all_errors = get_registered_errors();
for error in all_errors {
    println!("Error: {} - {}", error.type_name, error.error_code);
    for variant in &error.variants {
        println!("  Variant: {} ({})", variant.name, variant.code);
    }
}

// Get error statistics  
let stats = get_error_stats();
println!("Total error types: {}", stats.total_error_types);
println!("Total variants: {}", stats.total_variants);
println!("Components: {:?}", stats.components);
println!("Categories: {:?}", stats.categories);

// Export to JSON for documentation
let json = apollo_router_error::export_error_registry_json()?;
std::fs::write("error_registry.json", json)?;
```

## Build-Time Documentation Generation

You can generate comprehensive error documentation at build time:

```rust
// build.rs
use apollo_router_error::{get_registered_errors, export_error_registry_json};

fn main() {
    // Generate JSON documentation
    let json = export_error_registry_json().unwrap();
    std::fs::write("target/error_registry.json", json).unwrap();
    
    // Generate Markdown documentation
    let mut docs = String::from("# Error Registry\n\n");
    
    for error in get_registered_errors() {
        docs.push_str(&format!("## {}\n\n", error.type_name));
        docs.push_str(&format!("**Component**: {}\n", error.component));
        docs.push_str(&format!("**Category**: {}\n\n", error.category));
        
        for variant in &error.variants {
            docs.push_str(&format!("### {}\n", variant.name));
            docs.push_str(&format!("**Code**: `{}`\n", variant.code));
            if let Some(help) = variant.help {
                docs.push_str(&format!("**Help**: {}\n", help));
            }
            docs.push_str("\n");
        }
    }
    
    std::fs::write("target/ERROR_REGISTRY.md", docs).unwrap();
}
```

## Integration with Apollo Router Core

The derive macro generates a `RouterError` trait implementation. To use it with Apollo Router Core, import the trait:

```rust
use apollo_router_core::error::RouterError;

#[derive(Error, Diagnostic, Debug, RouterError)]
pub enum MyServiceError {
    // ... your error variants
}

// Your error implements RouterError automatically
let error = MyServiceError::ConfigError {
    message: "Invalid port".to_string(),
    config_source: Some("port: invalid".to_string()),
};

// Use with Apollo Router Core's error handling
let error_code = error.error_code(); // "apollo_router::my_service::config_error"

let mut extensions = std::collections::HashMap::new();
error.populate_graphql_extensions(&mut extensions);
// extensions now contains structured error data
```

## Features

The crate supports optional features:

- **`registry`**: Enables automatic error registration with the global error registry using `linkme`. When disabled, only the `RouterError` trait implementation is generated.

To enable registry features:

```toml
[dependencies]
apollo-router-error-derive = { version = "0.1", features = ["registry"] }
apollo-router-error = "0.1"  # Required when using registry feature
```

## Advanced Usage

### Custom GraphQL Extensions

While the derive macro handles most cases automatically, you can override the GraphQL extensions generation if needed:

```rust
impl MyServiceError {
    // This will be called by the generated populate_graphql_extensions method
    fn custom_graphql_details(&self, details: &mut HashMap<String, serde_json::Value>) {
        match self {
            Self::ConfigError { message, .. } => {
                // Add custom fields beyond what the macro generates
                details.insert("severity".to_string(), "high".into());
                details.insert("recoverable".to_string(), true.into());
            },
            _ => {} // Use macro-generated behavior for other variants
        }
    }
}
```

### Error Code Validation

You can validate error codes at compile time:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn validate_error_codes() {
        // Ensure all error codes follow the convention
        for error in apollo_router_error::get_registered_errors() {
            for variant in &error.variants {
                assert!(
                    variant.code.starts_with("apollo_router::"),
                    "Error code must start with apollo_router::"
                );
                assert!(
                    variant.code.matches("::").count() >= 2,
                    "Error code must have at least component::category"
                );
            }
        }
    }
}
```

## Requirements

- All error variants must have `#[diagnostic(code(...))]` attributes
- Error codes must follow the `apollo_router::{component}::{category}` format
- The error enum must also derive `Error` and `Diagnostic` from thiserror and miette

## Limitations

- Only works with `enum` types, not `struct` types
- Error codes are extracted from compile-time attributes, not runtime values
- GraphQL extensions generation is based on field names and types, not semantic meaning 