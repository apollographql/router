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

- **TTL** comes from the upstream `Cache-Control` header (`max-age`/`s-maxage`, adjusted for `Age`), falling back to the configured `ttl` when the REST API sends no `Cache-Control` header. That fallback governs the router's own storage only: a response with no `Cache-Control` header contributes `no-store` to the client-facing `Cache-Control` header, so the router never advertises a lifetime to clients or CDNs that the API did not ask for. `ttl` is therefore required configuration wherever connector caching is enabled with Redis.
- **Per-user caching** via `private_id` when a response is `Cache-Control: private`.
- **Invalidation** through the existing `/invalidation` endpoint. Connector entries are addressed with `sources` (for `cache_tag` requests) or the `connector`/`type`-with-`source` kinds, and are authorized by the connector source's own shared key — separately from subgraph invalidation.
- **Not cached:** mutations and client-batched GraphQL requests.

**Customization on a cache hit.** Connector root fields are cached at the connector HTTP request level, so a coprocessor's `ConnectorRequest`/`ConnectorResponse` stages still run when a root field is served from cache; the `ConnectorResponse` payload then carries `cacheHit: true` and no `statusCode` or `headers`, since no HTTP call was made. Connector *entity* fetches are cached above the point where one GraphQL fetch fans out into HTTP requests, so a fully cached entity fetch runs no connector coprocessor stages at all, and no connector-level telemetry, header propagation, traffic shaping or request limits. Rhai has no connector hook in either case. See [Response Cache Customization](https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/customization).

Connector and subgraph caching can share a Redis instance without collision: connector cache keys and their invalidation indexes are namespaced separately from subgraph entries.

Off by default; existing deployments see no behavior change. In particular, a config that sets only `response_cache.connector` (a connectors-only deployment) does not implicitly enable subgraph-side caching.

By [@andrewmcgivery](https://github.com/andrewmcgivery), [@TylerBloom](https://github.com/TylerBloom), [@briannafugate408](https://github.com/briannafugate408) and [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9171
