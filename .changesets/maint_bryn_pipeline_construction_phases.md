### Pipeline construction runs in explicit acquire, activate, and assemble phases ([PR #TODO](https://github.com/apollographql/router/pull/TODO))

The router now builds its request pipeline in three phases: acquire every resource whose creation can fail (plugins, TLS material, Redis connections, the persisted-query manifest), activate the plugins, then assemble the service stacks through infallible steps. Traces of startup and hot reload gain `acquire`, `activate`, and `assemble` spans under `starting`, so a slow reload shows which step is responsible.

This fixes a hot-reload defect: the automatic-persisted-query cache connected to Redis after activation had already swapped the global telemetry providers, so a failed connection (with `required_to_start: true`) left the router serving with broken metrics. The connection attempt now happens before activation; when it fails, the reload fails cleanly and the router keeps serving the previous configuration with working telemetry.

By [@BrynCooke](https://github.com/BrynCooke)
