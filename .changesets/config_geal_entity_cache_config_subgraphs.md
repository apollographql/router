### Align entity caching configuration structure for subgraph overrides ([PR #5474](https://github.com/apollographql/router/pull/5474))

Aligns the entity cache configuration structure to the same `all`/`subgraphs` override pattern found in other parts of the router configuration. For example, see the [header propagation](https://www.apollographql.com/docs/router/configuration/header-propagation) configuration.
An automated configuration migration is provided so existing usage is unaffected.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5474