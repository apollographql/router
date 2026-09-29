### Resolve `traffic_shaping` `pool_idle_timeout` from `all` and the documented default

`pool_idle_timeout` now resolves the way the traffic shaping documentation describes, for both subgraphs and connector sources:

- A `subgraphs.<name>` or `connector.sources.<name>` block that doesn't set `pool_idle_timeout` inherits the value from the matching `all` block. Previously, any such block silently replaced `all.pool_idle_timeout` with 15s.
- Without an `all` block, a subgraph or source that isn't configured uses the 15s default. Previously its pooled connections never expired.
- An explicit `pool_idle_timeout: null` in a per-subgraph or per-source block disables idle eviction for it. Previously it was ignored in favor of the `all` block's value.

```yaml
traffic_shaping:
  all:
    pool_idle_timeout: 5s
  subgraphs:
    products:
      experimental_http2: disable # now uses 5s from `all`, instead of 15s
```

By [@bryncooke](https://github.com/bryncooke)
