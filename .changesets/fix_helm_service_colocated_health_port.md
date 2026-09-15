### Fix Helm `Service` rendering a duplicate port when the health check listener shares the main port ([PR #10020](https://github.com/apollographql/router/pull/10020))

When `router.configuration.health_check.listen` is configured on the same port as `service.port`, the chart rendered two `Service` port entries with the same `(port, protocol)` pair. Kubernetes rejects that on a fresh apply:

```
Service "apollo-router" is invalid: spec.ports[1]: Duplicate value:
{"Name":"","Protocol":"TCP","AppProtocol":null,"Port":22216,"TargetPort":0,"NodePort":0}
```

The `health` port entry is now only rendered when it differs from `service.port`. In the co-located case the health endpoint stays reachable through the main `http` port; the default split setup (health check on `8088`) renders exactly as before.

By [@iksinski](https://github.com/iksinski) in https://github.com/apollographql/router/pull/10020
