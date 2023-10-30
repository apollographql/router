# Unstable Plugin API

The Plugin API is currently stable. However, we have noticed that there are some areas that could be improved.
To prevent the risk that we introduce a new API that does not fully meet our needs and our users needs we need to introduce an API for unstable and private hook points.

The order of introduction of new hook points would be:
* Private - Does this work and make the router code cleaner?
* Unstable - Available for use by plugins, but not guaranteed to be stable.
* Public - Usable by all, and won't change.

We will promote hook points from private to unstable to public as we gain confidence in them.

## Hook points that could be introduced

1. Query planning - Allows plugins to implement caching, or other optimizations.
2. Validation - Allows plugins to place additional constraints on the query before hitting the query planner.
3. Parse - Allows plugins to modify the query before it is parsed. e.g. APQ or persisted queries.
4. Http subgraph request - Allows signing of subgraph requests to take place.
5. Activate - Allow mutation of global state where needed.

## Proposed API

Introduce 2 new traits:

* pub PluginUnstable
* pub (crate) PluginPrivate

Demo here: https://play.rust-lang.org/?version=stable&mode=debug&edition=2021&gist=b4022dfeaeb920bc7ae94a94b7b9ea23

This doesn't give us permission to not think about APIs we introduce, but it does enable us to unblock ourselves and our users in a safe way.

## Acceptance criteria
* Existing plugins should compile without modification.
* Plugins external to the Router codebase cannot use the private API.
* Plugins external to the router codebase can use the unstable API.
* The new traits are documented appropriately.
