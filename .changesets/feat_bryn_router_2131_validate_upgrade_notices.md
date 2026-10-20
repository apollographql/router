### `router config validate` lists automatic upgrades in its output instead of logging them

When a valid configuration file relies on settings that the router upgrades automatically at startup, `router config validate` now lists each upgrade in its own output and suggests the `router config upgrade` command that updates the file:

```
Configuration at path router.yaml is valid.
It relies on automatic upgrades that the router applies at startup:
  - telemetry.apollo.otlp_tracing_sampler has been removed. Router 3 uses OTLP exclusively for traces. Remove this key from your config.
Run `router config upgrade router.yaml` to update the file.
```

Previously, each upgrade's notice was printed as a log line, which could be a `WARN` or `ERROR` line above a successful result and fail CI jobs that reject error logs. The command still exits successfully for these files. Startup and reload still log the notices as before.

By [@BrynCooke](https://github.com/BrynCooke)
