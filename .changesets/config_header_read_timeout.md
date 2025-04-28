### Add configurable server header read timeout ([PR #7262](https://github.com/apollographql/router/pull/7262))

This change exposes the server's header read timeout as the `server.http.header_read_timeout` configuration option.

By default, the `server.http.header_read_timeout` is set to previously hard-coded 10 seconds. A longer timeout can be configured using the `server.http.header_read_timeout` option.

```yaml title="router.yaml"
server:
  http:
    header_read_timeout: 30s
```

By [@gwardwell ](https://github.com/gwardwell) in https://github.com/apollographql/router/pull/7262
