### Clarify that hot-reload does not affect Uplink-delivered config/schema ([PR #3596](https://github.com/apollographql/router/pull/3596))

This documentation adjustment (and small CLI help change) tries to clarify some confusion around the `--hot-reload` command line argument and the scope of it's operation.

Concretely, the supergraph and configuration that is delivered through a [GraphOS Launch](https://www.apollographql.com/docs/graphos/delivery/launches/) (and delivered through Uplink) is _always_ loaded immediately and will take effect as soon as possible.

On the other hand, files that are provided locally - e.g., `--config ./file.yaml` and `--supergraph ./supergraph.graphql` - are only reloaded:

- If `--hot-reload` is passed (or if another flag infers `--hot-reload`, as is the case with `--dev`) and a supergraph or configuration is changed; or
- When the router process is sent a SIGHUP.

Otherwise, files provided locally to the router are only re-started if the router process is completely restarted.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/3596