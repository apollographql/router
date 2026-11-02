### `router config validate` lists automatic upgrades in its output instead of logging them

When a valid configuration file relies on automatic upgrades, `router config validate` now lists them and suggests running `router config upgrade`:

```
Configuration at path router.yaml is valid.
It relies on automatic upgrades that the router applies at startup:
  - telemetry.apollo.otlp_tracing_sampler has been removed. Router 3 uses OTLP exclusively for traces. Remove this key from your config.
Run `router config upgrade router.yaml` to update the file.
```

Previously it logged these notices, so a valid file could print `WARN` or `ERROR` lines and fail CI jobs that reject error logs. The exit status is unchanged, and startup and reload still log the notices.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10605
