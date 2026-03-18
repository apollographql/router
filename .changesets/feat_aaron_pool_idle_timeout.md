### Add configurable `pool_idle_timeout` for HTTP client connection pools ([PR #9014](https://github.com/apollographql/router/pull/9014))

Adds a new `pool_idle_timeout` configuration option to the HTTP client used by subgraphs, connectors, and coprocessors. This controls how long idle keep-alive connections remain in the connection pool before being evicted. The default is 15 seconds (up from the previous hardcoded 5 seconds). Setting it to `null` disables the idle eviction interval entirely, meaning pooled connections are never evicted due to idleness.

The option is available at every level where HTTP client configuration applies:

```yaml
traffic_shaping:
  all:
    pool_idle_timeout: 30s      # applies to all subgraphs
  subgraphs:
    products:
      pool_idle_timeout: 60s    # per-subgraph override
  connector:
    all:
      pool_idle_timeout: 30s    # applies to all connectors
    sources:
      my_source:
        pool_idle_timeout: 60s  # per-source override

coprocessor:
  url: http://localhost:8081
  client:
    pool_idle_timeout: 30s      # coprocessor client
```

By [@aaronarinder](https://github.com/aaronarinder) in https://github.com/apollographql/router/pull/9014
