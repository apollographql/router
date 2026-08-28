### Response caching for Apollo Connectors

Response caching now applies to Apollo Connectors, not just GraphQL subgraph fetches. Connector (REST/HTTP) responses can be cached in Redis and reused across requests, driven by the same `Cache-Control` semantics as the existing subgraph feature. Configure it under a new `response_cache.connector` block, keyed by connector source (`subgraph_name.source_name`):

```yaml title="router.yaml"
response_cache:
  enabled: true
  connector:
    all:
      enabled: true
      ttl: 60s
      redis:
        urls: ["redis://..."]
    sources:
      products.ecom_api:
        ttl: 5m
```

Behavior mirrors subgraph response caching:

- **TTL** comes from the upstream `Cache-Control` header (`max-age`/`s-maxage`, adjusted for `Age`), falling back to the configured `ttl` when the REST API sends no `Cache-Control` header.
- **Per-user caching** via `private_id` when a response is `Cache-Control: private`.
- **Invalidation** through the existing `/invalidation` endpoint. Connector entries are addressed with `sources` (for `cache_tag` requests) or the `connector`/`type`-with-`source` kinds, and are authorized by the connector source's own shared key — separately from subgraph invalidation.
- **Not cached:** mutations and client-batched GraphQL requests.

Connector and subgraph caching can share a Redis instance without collision: connector cache keys and their invalidation indexes are namespaced separately from subgraph entries.

Off by default; existing deployments see no behavior change. In particular, a config that sets only `response_cache.connector` (a connectors-only deployment) does not implicitly enable subgraph-side caching.

By [@andrewmcgivery](https://github.com/andrewmcgivery) and [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/9171
