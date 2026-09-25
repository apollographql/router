### Support Federation v3.0 ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

The router now supports subgraphs that link the Federation v3.0 spec:

```graphql
extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key"])
```

Federation v3.0 includes every Federation v2.x feature, so moving a subgraph from a v2.x link to `federation/v3.0` keeps its existing directives. Subgraphs on v2.x and v3.0 can be composed together into one supergraph.

Federation v3.0 is the minimum federation version required by the preview `connect/v0.5` spec.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/PULL_NUMBER
