### Experimental logging of broken pipe errors ([PR #4870](https://github.com/apollographql/router/pull/4870))

You can now emit a log message each time the client closes the connection early, which can help you debug issues with clients that close connections before the server can respond. 

This feature is disabled by default but can be enabled by setting the `experimental_log_broken_pipe` option to `true`:

```yaml title="router.yaml"
supergraph:
  experimental_log_on_broken_pipe: true
```

Users that have internet facing routers will likely not want to opt in to this log message as they have no control over the clients.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4770 and [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4870 
