### `response_cache` can now emit a `Cache-Tag` header for CDN-side purging ([Issue #9481](https://github.com/apollographql/router/issues/9481))

If you cache router responses at a CDN or reverse proxy in front of the router, the CDN previously had no way to know *what* to purge when your underlying data changed — `Cache-Control` only governs freshness (TTL), not invalidation. A new opt-in `response_cache.cdn_invalidation` block closes that gap: the router emits a response header carrying the same invalidation labels it already uses for active invalidation against its own Redis cache, so you can purge your CDN's edge cache using the CDN's own tag-based purge API.

```yaml title="router.yaml"
response_cache:
  enabled: true
  cdn_invalidation:
    enabled: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: ["redis://..."]
```

The header's value is a delimited list of labels drawn from the same sources already used for Redis invalidation (`@cacheTag` directives and the `apolloCacheTags`/`apolloEntityCacheTags` response extensions) — no schema changes required if you've already set those up. Each label is one of three tiers, coarsest to finest:

- `subgraph-{name}` — every subgraph touched by the response.
- `type-{subgraph}-{type}` — every distinct `(subgraph, GraphQL type)` pair touched.
- the exact tag value from `@cacheTag`/the extensions, unchanged.

All three tiers are always sent together (subject to the size limit below), so you can escalate from purging a single fine-grained tag up to an entire subgraph's cached data if you're not confident a narrower purge fully propagated across every CDN edge location.

This is independent of the router's Redis-backed invalidation indexes: you don't need Redis response caching enabled, and you don't need the `cache_tag` invalidation index on, for the header to work correctly — including on a cache hit, since the labels needed to rebuild the header are persisted alongside the cached entry.

Configuration:

```yaml title="router.yaml"
response_cache:
  cdn_invalidation:
    enabled: true
    header_name: "Cache-Tag"          # e.g. "Surrogate-Key" for Fastly
    header_delimiter: ","             # e.g. " " for Fastly
    max_bytes: 16384                  # matches Cloudflare's default Cache-Tag limit
    experimental_drop_on_overflow: false
```

When a response's full label set would exceed `max_bytes`, the router packs the header coarsest-first and drops whatever doesn't fit, finest-grained first — so an oversized response still gets a usable header rather than none at all. That default favors availability over precision: the response still gets cached at the CDN, just with a coarser invalidation surface than it ideally would have.

For cases where that tradeoff isn't acceptable — where caching data you can't fully invalidate by its intended fine-grained tag is worse than not caching it at all — set `experimental_drop_on_overflow: true`. This omits the header entirely whenever truncation would otherwise occur, so you can pair it with a CDN-side rule that forces cache bypass on a missing header, guaranteeing you never end up with cached data whose purge surface is narrower than what your invalidation logic expects. It's marked experimental because it's a deliberate, opt-in safety valve rather than the router's default posture, and its shape may still change.

With `response_cache.debug: true`, the cache debugger now also shows the labels behind each cache entry and, per response, the `Cache-Tag` header's outcome, value, untruncated size, and whether it was actually emitted. Three new metrics (`cdn_tag_header.outcome`, `cdn_tag_header.untruncated_size_bytes`, `cdn_tag_header.error`) track header emission, truncation, and errors.

Off by default; existing deployments see no behavior change.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9811
