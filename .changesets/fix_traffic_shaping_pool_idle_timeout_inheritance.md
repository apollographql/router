### Apply the documented `pool_idle_timeout` default and inherit it from `traffic_shaping.all`

`pool_idle_timeout` now resolves as documented:

- A `subgraphs.<name>` or `connector.sources.<name>` block that omits it uses the value from `all` (or `connector.all`). Previously it got 15s.
- Subgraphs and connector sources with no matching block and no `all` block, and coprocessors with no `client` block, get the 15s default. Previously their idle connections never expired.
- `null` in a per-subgraph or per-source block disables idle eviction for it. Previously the `all` value was used.

```yaml
traffic_shaping:
  all:
    pool_idle_timeout: 5s
  subgraphs:
    products:
      http2: disable # now uses 5s from `all`, instead of 15s
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10315
