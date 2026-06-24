### Add per-subgraph `indexes` configuration to `response_cache` invalidation ([Issue #9521](https://github.com/apollographql/router/issues/9521))

Adds a new `indexes` block under each subgraph's `response_cache.subgraph.<name>.invalidation` configuration, letting operators choose which invalidation indexes Apollo Router maintains in Redis for that subgraph. All three indexes are enabled by default, so existing deployments are unchanged.

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
        indexes:                # all three default to true; omit fields you want kept on
          subgraph: false       # disable `By subgraph` invalidation for this subgraph
          type: false           # disable `By type` invalidation for this subgraph
          # cache_tag inherits its default (true) and continues to be honored
    subgraphs:
      networkapi_subgraph:
        invalidation:
          enabled: true
          indexes:
            type: false         # mix per subgraph; other fields inherit their defaults
```

When a subgraph's `indexes` block disables a mode, the corresponding ZSET writes are skipped on cache inserts and the `/invalidation` endpoint returns HTTP 400 with a structured error for requests of that kind. Operators with workloads that only ever invalidate by a subset of modes can use this to tailor `response_cache`'s indexing to their access pattern.

**Index changes are additive only.** Enabling a previously-disabled index does not retroactively populate it for entries that were written under the prior configuration. If a deployment changes `indexes.subgraph` from `false` to `true`, the `subgraph-{name}` ZSET will only see entries written after the change; pre-change entries are invisible to `By subgraph` invalidation requests until they age out via TTL. To bring a newly-enabled index online over the full cache set, flush Redis (or the affected namespace) before turning the index on.

By [@ebylund](https://github.com/ebylund) in [PR #9531](https://github.com/apollographql/router/pull/9531)
