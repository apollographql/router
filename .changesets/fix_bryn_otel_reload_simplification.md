### Reload telemetry only when configuration changes ([PR #8328](https://github.com/apollographql/router/pull/8328))

Previously, schema or config reloads would always reload telemetry, dropping existing exporters and creating new ones.

Telemetry exporters are now only recreated when relevant configuration has changed.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8328
