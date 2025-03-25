### Connection shutdown timeout 1.x ([PR #7058](https://github.com/apollographql/router/pull/7058))

When a connection is closed we call `graceful_shutdown` on hyper and then await for the connection to close.

Hyper 0.x has various issues around shutdown that may result in us waiting for extended periods for the connection to eventually be closed.

This PR introduces a configurable timeout from the termination signal to actual termination, defaulted to 60 seconds. The connection is forcibly terminated after the timeout is reached.

To configure, set the option in router yaml. It accepts human time durations:
```
supergraph:
  connection_shutdown_timeout: 60s
```

Note that even after connections have been terminated the router will still hang onto pipelines if `early_cancel` has not been configured to true. The router is trying to complete the request. 

Users can either set `early_cancel` to `true` 
```
supergraph:
  early_cancel: true
```

AND/OR use traffic shaping timeouts:
```
traffic_shaping:
  router:
    timeout: 60s
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7058
