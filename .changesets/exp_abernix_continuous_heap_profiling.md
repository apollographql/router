### Continuously profile router memory in Datadog

The router can now continuously profile its own memory use and send the profiles to Datadog's Continuous Profiler, so you can see which code paths hold memory over time. It works in any container image, including distroless images and read-only root filesystems, with no external profiler and no extra container privileges.

```yaml title="router.yaml"
experimental_continuous_profiling:
  heap:
    enabled: true
  exporters:
    datadog:
      enabled: true
```

- Profiles are sent through your Datadog Agent (7.20 or later), which the router finds the same way as for Datadog tracing (`DD_AGENT_HOST` or `DD_TRACE_AGENT_URL`). They're tagged with `DD_SERVICE` (default `router`), `DD_ENV`, and `DD_VERSION`.
- A profile is sent every minute by default; set `interval` to change it.
- Profiling covers memory allocated after it starts, and works on Linux and macOS.
- If the Agent can't be reached, the router drops that profile, logs a warning, and keeps serving traffic. The `apollo.router.profiling.exports` metric counts uploads by outcome.

Off by default. Turning it on or off takes effect on a configuration reload.

By [@abernix](https://github.com/abernix)
