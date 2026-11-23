### HTTP/2 to subgraphs, connectors and coprocessors is off by default ([PR #10850](https://github.com/apollographql/router/pull/10850))

The router now uses HTTP/1.1 for requests to subgraphs, connector sources and coprocessors unless you configure HTTP/2. Previously an unset `http2` behaved as `enable`: the router offered HTTP/2 over TLS and used it whenever the server supported it. Plain-HTTP connections are unaffected, because they only used HTTP/2 with `http2: http2only`.

You need to act if a subgraph, connector source or coprocessor accepts only HTTP/2 over TLS, or if you set `experimental_http2_keep_alive_interval`, which sends pings only over HTTP/2. Otherwise expect more open connections under load, because HTTP/1.1 sends one request at a time on each connection.

To keep the previous behavior, set `http2: enable` for each client that should use HTTP/2:

```yaml title="router.yaml"
traffic_shaping:
  all:
    http2: enable
  connector:
    all:
      experimental_http2: enable
coprocessor:
  url: https://coprocessor.example.com
  client:
    http2: enable
```

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10850
