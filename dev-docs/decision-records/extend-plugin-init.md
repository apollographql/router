
# Title [ADR-3]

Extend the `PluginInit` structure

## Status

Approved

## Context

`PluginInit` was created to allow us to add new public and private information to plugin initialization in a non breaking way.

We currently have some code that would benefit from extending the current information on PluginInit, allowing a cleaner separation of concerns.

### Areas that could be improved

1. Some plugins need to know when to change global resources. Currently, plugins cannot know when this needs to happen as they are unaware of the configuration pre reload.
2. Plugins are not aware of if licensed functionality is available.
3. Plugins do not know if APOLLO_KEY and APOLLO_GRAPH have been set, and we have some hacky code to inject it where we need it.
4. Plugins do not have access to parsed supergraph.


### Current Structure

```rust 
pub struct PluginInit<T> {
    /// Configuration
    pub config: T,
    /// Router Supergraph Schema (schema definition language)
    pub supergraph_sdl: Arc<String>,

    pub(crate) notify: Notify<String, graphql::Response>,
}
```

## Decision

Let's extend the structure to address the areas requiring improvement.

### Updated Structure

```rust 
pub struct PluginInit<T> {
    /// Configuration
    pub config: T,
    /// Configuration from last successful reload unless this is the first load.
    pub last_config: Option<T>,
    /// Router Supergraph Schema (schema definition language)
    pub supergraph_sdl: Arc<String>,

    /// The root configuration object of the router.
    pub (crate) root_configuration: Arc<Configuration>,
    /// True it the router was started with a valid license.
    pub (crate) licensed: bool,
    /// The parsed supergraph.
    pub (crate) supergraph: Arc<Schema>,

    /// APOLLO_KEY if set
    pub (crate) apollo_key: Option<String>,
    /// APOLLO_GRAPH_REF if set
    pub (crate) apollo_graph_ref: Option<String>,

    
    pub(crate) notify: Notify<String, graphql::Response>,
}
```

## Consequences

Plugins can now access the information which they require to operate effectively.
Various hacks/workaround can be replaces with cleaner implementations.

