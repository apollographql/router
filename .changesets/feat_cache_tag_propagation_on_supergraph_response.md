### Propagate `response_cache` cache tags to the supergraph response ([Issue #9481](https://github.com/apollographql/router/issues/9481))

Adds a new opt-in `propagate_cache_tags` block under `response_cache` that emits the aggregated set of cache tags involved in a request as a configurable response header on the supergraph response. This lets a downstream CDN (Cloudflare, Fastly, others) perform tag-based purging keyed on the same tags Apollo Router already maintains internally for `By cache tag` invalidation, so router-side and CDN-side caches can be invalidated in step.

The aggregated tag set is also exposed on the request context as a typed extension, available to coprocessor and rhai consumers without additional plumbing.

```yaml
response_cache:
  enabled: true
  propagate_cache_tags:
    enabled: true               # opt-in; default off
    header: "Cache-Tag"         # configurable; common alternates: "Surrogate-Key" (Fastly)
    separator: ","              # how to join multiple tags
    max_bytes: 16384            # cap per CDN limits
    on_overflow: "truncate"     # "truncate" | "drop" | "header_per_tag"
```

Behavior summary:

- Off by default. Existing deployments see no behavior change.
- Tags are sorted lexicographically and deduplicated before emission for deterministic output.
- An empty aggregated tag set suppresses the header rather than emitting an empty value.
- Overflow above `max_bytes` follows `on_overflow`: `truncate` (default), `drop`, or `header_per_tag`. Truncate and drop log a structured `tracing::warn!` with `max_bytes`, `actual_bytes`, and `dropped_count` so operators can monitor sizing.
- Cache tags are populated on every cache hit, including non-debug production hits, so the aggregated set is identical between cache-miss and cache-hit responses. Legacy entries written before this change carry no tag set and contribute zero tags until they age out via TTL.
- Internal `__apollo_internal::`-prefixed tag entries are filtered out at the aggregator boundary; only user-facing tags (from `apolloCacheTags`, `apolloEntityCacheTags`, and resolved `@cacheTag` directive values) flow into the header.

By [@ebylund](https://github.com/ebylund) in PR
