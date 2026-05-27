### Add opt-in `index_modes` to `response_cache` invalidation config ([Issue #9521](https://github.com/apollographql/router/issues/9521))

`response_cache` previously maintained Redis ZSET indexes for all three invalidation modes (`by subgraph`, `by type`, `by cache tag`) on every cache insert, regardless of which modes a deployment actually used. Customers who only invalidate by cache tag paid continuous Redis CPU and memory cost for indexes they never queried.

The new per-subgraph `index_modes` setting under `response_cache.subgraph.<all|subgraphs.NAME>.invalidation` lets operators opt out of unused index modes. The field defaults to all three modes (`["subgraph", "type", "cache_tag"]`) for backward compatibility, so existing deployments are unchanged.

```yaml
response_cache:
  enabled: true
  invalidation:
    listen: "127.0.0.1:3000"
    path: "/invalidation"
  subgraph:
    all:
      enabled: true
      invalidation:
        enabled: true
        shared_key: "${env.INVALIDATION_SHARED_KEY}"
        index_modes: ["cache_tag"]   # default: ["subgraph", "type", "cache_tag"]
    subgraphs:
      networkapi_subgraph:
        invalidation:
          enabled: true
          index_modes: ["cache_tag", "type"]   # mix per subgraph
```

When a request to `/invalidation` targets a subgraph whose `index_modes` does not include the requested kind, the endpoint returns HTTP 400 with a structured error: `invalidation kind '<kind>' is not enabled for subgraph '<name>'; index_modes does not include this kind`. Surfaces misconfiguration to callers quickly rather than no-oping.

`index_modes: []` is a supported configuration meaning "pure TTL-based caching with no invalidation API"; all per-insert ZSET writes are eliminated and every `/invalidation` request returns 400.

By [@ebylund](https://github.com/ebylund) in PR
