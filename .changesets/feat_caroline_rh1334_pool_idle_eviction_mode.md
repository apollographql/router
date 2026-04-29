### Add `pool_idle_eviction_mode` configuration for connection pool eviction ([PR #XXXX](https://github.com/apollographql/router/pull/XXXX))

A new `pool_idle_eviction_mode` option is available in `traffic_shaping` configuration (for both subgraphs and connectors) and in the coprocessor HTTP client config.

**Background:** When `pool_idle_timeout` was introduced, the router unconditionally enabled a background timer (`pool_timer`) that proactively closes idle connections when they exceed the timeout. In some network environments, the TCP close sent by this background task races with a new connection attempt and causes significant latency spikes on the next request.

**New option:** `pool_idle_eviction_mode` controls when expired connections are evicted:

- `lazy` **(default)**: connections are only evicted at checkout time, when a request finds the pooled connection has exceeded `pool_idle_timeout`. No background TCP closes are sent between requests. This matches router behavior before v2.13.0.
- `active`: a background timer proactively drops expired connections, matching the behavior introduced in v2.13.0.

```yaml
traffic_shaping:
  all:
    pool_idle_timeout: 30s
    pool_idle_eviction_mode: lazy  # default; use "active" to restore previous behavior
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
