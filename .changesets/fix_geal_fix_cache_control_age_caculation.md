### Fix Cache-Control aggregation and age calculation in entity caching ([PR #5463](https://github.com/apollographql/router/pull/5463))

Enhances the reliability of caching behaviors in the entity cache feature by:

- Ensuring the proper calculation of `max-age` and `s-max-age` fields in the `Cache-Control` header sent to clients.
- Setting appropriate default values if a subgraph does not provide a `Cache-Control` header.
- Guaranteeing that the `Cache-Control` header is aggregated consistently, even if the plugins is disabled entirely or on specific subgraphs.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5463