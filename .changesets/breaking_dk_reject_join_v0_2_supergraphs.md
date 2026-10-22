### Supergraphs composed with Federation 2.0–2.2 are no longer supported

The router now refuses to start with a supergraph that uses join spec v0.2 (`https://specs.apollo.dev/join/v0.2`), the same way it already rejects supergraphs composed with Federation 1 (join spec v0.1). Federation 2.0 through 2.2 compose supergraphs with join spec v0.2. Federation 2.3 and later use v0.3 or newer, which is now the minimum the router supports.

What to do: recompose your supergraph with Federation 2.3 or later, for example by setting `federation_version` in your `rover supergraph compose` config or by updating the build pipeline's federation version in GraphOS.

When the router rejects a supergraph for this reason, the `apollo.router.lifecycle.query_planner.init` metric records `init.error_kind = unsupported_join_version`.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/TBD
