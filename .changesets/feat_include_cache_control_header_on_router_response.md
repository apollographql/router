### Add `include_cache_control_header_on_router_response` config option to suppress `Cache-Control` on client responses ([PR #9002](https://github.com/apollographql/router/pull/9002))

The response cache plugin now supports a `include_cache_control_header_on_router_response` boolean config option (defaults to `true`). When set to `false`, the router omits the `Cache-Control` header from supergraph responses sent to clients, while all internal caching behavior — Redis storage, TTL enforcement, cache key computation, and the cache debugger — remains unchanged.

This is useful when the router sits behind a CDN or reverse proxy that manages its own caching headers, or when you want to prevent clients from caching responses locally while keeping server-side caching active.

```yaml
response_cache:
  enabled: true
  include_cache_control_header_on_router_response: false  # default: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: ["redis://..."]
```

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9002
