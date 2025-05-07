# Apollo Router Plugin System

## Overview

Apollo Router supports a modular plugin system that allows you to customize and extend its behavior at various stages of request processing. Plugins are compiled into the router and configured via YAML configuration. Each plugin can hook into different phases of the request lifecycle, such as authentication, authorization, caching, telemetry, and more.

Plugins are organized as Rust modules in this directory. Many plugins are enabled or configured through the router's YAML configuration file, allowing you to tailor the router's behavior to your needs.

## How the Plugin System Works

- **Registration:** Plugins are registered in the router's codebase and are available for configuration.
- **Configuration:** Each plugin exposes a configuration schema, typically using YAML, which is validated at startup.
- **Lifecycle Hooks:** Plugins can hook into different stages, such as:
  - Router request/response
  - Supergraph request/response
  - Subgraph request/response
  - Connector request/response
- **Extensibility:** You can add your own plugins by following the structure of existing ones.

## Guide: Defining a Plugin

To define a new plugin in Apollo Router, follow these steps:

1. **Create a New Module:**  
   Create a new Rust module in the `apollo-router/src/plugins` directory. For example, create a file named `my_plugin.rs`.

2. **Implement the Plugin Trait:**  
   Your plugin should implement the `Plugin` trait. This trait defines methods such as `new`, `router_service`, `supergraph_service`, `execution_service`, and `subgraph_service`. These methods allow your plugin to hook into different stages of the request lifecycle.

   Example:
   ```rust
   use crate::plugin::Plugin;
   use crate::plugin::PluginInit;
   use crate::services::router;
   use crate::services::subgraph;

   pub struct MyPlugin {
       // Plugin state or configuration
   }

   #[async_trait::async_trait]
   impl Plugin for MyPlugin {
       type Config = MyPluginConfig;

       async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
           // Initialize your plugin with configuration
           Ok(MyPlugin {})
       }

       fn router_service(&self, service: router::BoxService) -> router::BoxService {
           // Modify the router service
           service
       }

       fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
           // Modify the subgraph service
           service
       }
   }
   ```

3. **Define Configuration:**  
   Define a configuration struct for your plugin using Serde and JsonSchema. This struct will be used to validate and deserialize the plugin's configuration from the YAML file.

   Example:
   ```rust
   use schemars::JsonSchema;
   use serde::Deserialize;

   #[derive(Clone, Debug, Deserialize, JsonSchema)]
   pub struct MyPluginConfig {
       // Configuration fields
       pub enabled: bool,
       pub timeout: Option<Duration>,
   }
   ```

4. **Register the Plugin:**  
   In the `mod.rs` file, register your plugin by adding a line like:
   ```rust
   pub(crate) mod my_plugin;
   ```

   Additionally, default plugins must be added to a list to be loaded. This is typically done using the `register_plugin!` macro, which registers the plugin with a group and a name. For example:
   ```rust
   register_plugin!("apollo", "my_plugin", MyPlugin);
   ```

5. **Documentation:**  
   Add documentation comments to your plugin module and configuration struct to explain its purpose and configuration options.

6. **Testing:**  
   Write tests for your plugin to ensure it behaves as expected.

7. **Configuration Example:**  
   Provide an example of how to configure your plugin in the router's YAML configuration file.

   Example YAML configuration:
   ```yaml
   plugins:
     my_plugin:
       enabled: true
       timeout: 30s
   ```

By following these steps, you can create a custom plugin that extends Apollo Router's functionality.

## Summary of Existing Plugins

Below is a summary of the main plugins included in Apollo Router:

### Authentication

- **Purpose:** Verifies incoming requests using JWTs or other mechanisms. Supports extracting tokens from headers or cookies, and can be configured to allow or reject unauthenticated requests.
- **Configurable:** Yes (JWT issuers, header names, error handling, etc.)

### Authorization

- **Purpose:** Enforces access control based on authentication status, scopes, or custom policies. Supports GraphQL directives like `@authenticated` and `@requiresScopes`.
- **Configurable:** Yes (require authentication, enable/disable directives, error handling)

### Cache

- **Purpose:** Controls HTTP and entity caching, cache invalidation, and cache metrics. Supports fine-grained cache control headers and invalidation endpoints.
- **Configurable:** Yes (cache control, invalidation rules, metrics)

### Telemetry

- **Purpose:** Collects and exports metrics and traces for observability. Integrates with OpenTelemetry, Prometheus, and Apollo Studio.
- **Configurable:** Yes (metrics exporters, tracing, custom instruments)

### Traffic Shaping

- **Purpose:** Manages request flow with features like query deduplication, timeouts, compression, and rate limiting.
- **Configurable:** Yes (enable/disable features, set limits, compression types)

### Headers

- **Purpose:** Allows insertion, removal, and propagation of HTTP headers at various stages. Supports static values, context-based, and body-based header manipulation.
- **Configurable:** Yes (rules for each stage and target)

### Coprocessor

- **Purpose:** Offloads processing to external HTTP services at various pipeline stages (router, supergraph, execution, subgraph). Useful for custom logic or integrations.
- **Configurable:** Yes (external URLs, which data to send, timeouts)

### Content Negotiation

- **Purpose:** Handles HTTP content negotiation using `Accept` and `Content-Type` headers. Ensures clients receive responses in supported formats.
- **Configurable:** Minimal (mostly internal logic)

### Subscription

- **Purpose:** Adds support for GraphQL subscriptions, including callback and passthrough modes, deduplication, and connection management.
- **Configurable:** Yes (modes, deduplication, limits)

### CSRF Protection

- **Purpose:** Protects against Cross-Site Request Forgery attacks by validating tokens in requests.
- **Configurable:** Yes (token validation, error handling)

### Demand Control

- **Purpose:** Manages request load by implementing rate limiting and load shedding strategies.
- **Configurable:** Yes (rate limits, load shedding rules)

### Enhanced Client Awareness

- **Purpose:** Provides additional context about clients, such as client name and version, for better observability.
- **Configurable:** Yes (client information extraction)

### File Uploads

- **Purpose:** Handles file uploads in GraphQL requests, supporting multipart form data.
- **Configurable:** Yes (upload limits, file types)

### Health Check

- **Purpose:** Provides endpoints for health checks to monitor the router's status.
- **Configurable:** Yes (endpoint paths, response format)

### License Enforcement

- **Purpose:** Enforces licensing rules, such as usage limits and feature restrictions.
- **Configurable:** Yes (license rules, error handling)

### Limits

- **Purpose:** Implements various limits on requests, such as query complexity and depth.
- **Configurable:** Yes (limit rules, error handling)

### Progressive Override

- **Purpose:** Allows progressive rollout of changes by overriding specific parts of the schema or configuration.
- **Configurable:** Yes (override rules, conditions)

### Record/Replay

- **Purpose:** Records and replays requests for testing and debugging purposes.
- **Configurable:** Yes (recording rules, replay conditions)

### Rhai

- **Purpose:** Integrates with the Rhai scripting language for custom logic and transformations.
- **Configurable:** Yes (script paths, execution rules)

---

**Other Plugins:**  
There are additional plugins for CSRF protection, demand control, enhanced client awareness, file uploads, health checks, license enforcement, limits, progressive override, record/replay, and more. Each is implemented as a Rust module and may have its own configuration.

## Adding or Configuring Plugins

To enable or configure a plugin, add the relevant section to your router's YAML configuration file. Refer to the documentation for each plugin for available options and schema.

## Contributing

To add a new plugin, create a new module in this directory, implement the required traits, and register it in `mod.rs`. Follow the patterns used by existing plugins for best practices.

---

Let us know if you want to include more detailed configuration examples or a table of plugins!
