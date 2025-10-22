### Only reload telemetry when needed ([PR #8328](https://github.com/apollographql/router/pull/8328))

Previously when schema or config reload took place telemetry would always be reloaded. This would drop existing exporters 
and create new ones.

Telemetry exporters will now only be recreated if relevant configuration has changed.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8328
