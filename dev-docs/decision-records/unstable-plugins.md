# Title [ADR-4]

Provide a mechanism which supports stable evolution of the Plugin APIs

## Status

Approved

## Context

The Plugin API is currently stable. However, we have noticed that there are some areas that could be improved.
To prevent the risk that we introduce a new API that does not fully meet our, and our users', needs we wish to introduce APIs for private and unstable plugin services.

Part of the motivation for this decision, was the recognition that we already had requirements for several possible new plugin services.

1. Query planning - Allows plugins to implement caching, or other optimizations.
2. Validation - Allows plugins to place additional constraints on the query before hitting the query planner.
3. Parse - Allows plugins to modify the query before it is parsed. e.g. APQ or persisted queries.
4. Http subgraph request - Allows signing of subgraph requests to take place.
5. Activate - Allow mutation of global state where needed.

## Decision

### Introduce New APIs

Introduce 2 new traits:

* pub (crate) PluginPrivate	
* pub PluginUnstable

[Demo](https://play.rust-lang.org/?version=stable&mode=debug&edition=2021&gist=b4022dfeaeb920bc7ae94a94b7b9ea23)

By default, new plugin services will start their life as Private, but it is possible to start as Unstable if required.

### Acceptance criteria

* Existing plugins should compile without modification.
* Plugins external to the Router codebase cannot use the private API.
* Plugins external to the router codebase can use the unstable API.
* The new traits are documented appropriately.

### Progression from Private to Public

The order of introduction of new service would be:
* Private - Does this work and make the router code cleaner?
* Unstable - Available for use by plugins, but not guaranteed to be stable.
* Public - Usable by all, and won't change.

We will promote plugin services from private to unstable to public as we gain confidence in them.

#### Graduation criteria

##### Private to Unstable

They do not need to be supported by all extensible aspects of the Router, for example: Rhai, coprocessors or telemetry. However, they should have documentation and tests.
private APIs can be promoted to unstable if they have been used in the Router for an internal plugin and an additional DR is raised for approved by the team. 

##### Unstable to Public

To promote to public, and unstable API must:
* Be supported in coprocessors (unless there is a really good reason not to).
* Be supported in Rhai (unless there is a really good reason not to).
* Have telemetry support.
Unstable APIs can be promoted to public if they have been used in the Router for an internal plugin and an additional DR is raised for approved by the team.

## Consequences

It will be easier to evolve the Plugin API.
The consequences of evolving the API will be evaluated in a structured fashion.
Interactions between changing components will become easier to spot.

