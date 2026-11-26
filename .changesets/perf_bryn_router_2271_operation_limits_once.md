### Operation limits are measured once per operation

The router measured every request's operation for depth, height, root fields and aliases, the values that `limits.router.max_depth`, `max_height`, `max_root_fields` and `max_aliases` are checked against. It did this even for routers with none of these limits set, because the values also feed telemetry. The router now measures an operation the first time it is parsed and reuses the result for later requests with the same query, which lowers CPU use per request for every router.

No configuration change is needed.

By [@BrynCooke](https://github.com/BrynCooke)
