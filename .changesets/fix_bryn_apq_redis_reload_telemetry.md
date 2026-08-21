### A failed APQ Redis connection during hot reload no longer breaks telemetry ([PR #10045](https://github.com/apollographql/router/pull/10045))

The automatic-persisted-query cache connected to Redis after a hot reload had already swapped the global telemetry providers, so a failed connection (with `required_to_start: true`) left the router serving with broken metrics. The connection attempt now happens before the swap; when it fails, the reload fails cleanly and the router keeps serving the previous configuration with working telemetry.

By [@BrynCooke](https://github.com/BrynCooke)
