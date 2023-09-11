### Subgraph authentication: Make sure Request signing happens after Compression and APQ ([Issue #3608](https://github.com/apollographql/router/issues/3608))

[Subgraph authentication](https://www.apollographql.com/docs/router/configuration/authn-subgraph) is available since router v1.27.0.

Unfortunately this first version didn't work well with features that operate with the SubgraphService, for example:
  - Subgraph APQ
  - Subgraph HTTP compression
  - Custom plugins that operate on the Subgraph level, written either via coprocessors, in rhai, or native.

The router will now sign subgraph requests just before they are sent to subgraphs.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3735
