### Support Federation v3.0 ([PR #10283](https://github.com/apollographql/router/pull/10283))

The router now supports subgraphs that link the Federation v3.0 spec:

```graphql
extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key"])
```

Federation v3.0 includes every Federation v2.x feature, so moving a subgraph from a v2.x link to `federation/v3.0` keeps its existing directives. Subgraphs using Federation v1, v2.x and v3.0 can be composed together into one supergraph: when any subgraph links Federation v3.0, the other subgraphs are automatically upgraded to v3.0 (the same way Federation v1 subgraphs are upgraded to v2), and the supergraph uses the new `join/v0.6` spec.

Federation v3.0 is the minimum federation version required by the preview `connect/v0.5` spec.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10283
