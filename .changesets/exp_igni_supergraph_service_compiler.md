### Expose the apollo compiler at the supergraph service level ([PR #3200](https://github.com/apollographql/router/pull/3200))

This adds a query analysis phase inside the router service, before sending the query through the supergraph plugins. It makes a compiler available to supergraph plugins, to perform deeper analysis of the query. That compiler is then used in the query planner to create the `Query` object containing selections for response formatting.

This is for internal use only for now, until we are sure we can expose the right public API.

By [@o0Ignition0o](https://github.com/o0Ignition0o) [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3200