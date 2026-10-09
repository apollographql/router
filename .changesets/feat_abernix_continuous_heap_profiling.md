### Add experimental continuous heap profiling, exported to Datadog

The router can now profile its own heap continuously and ship the profiles to a Datadog agent, without an external profiler or a special container image:

```yaml
experimental_continuous_profiling:
  heap:
    enabled: true
  interval: 60s
  exporters:
    datadog:
      enabled: true
      # Defaults to http://127.0.0.1:8126; DD_TRACE_AGENT_URL or DD_AGENT_HOST take precedence.
      endpoint: default
```

Profiles come from the router's jemalloc allocator, which samples allocations and records their call stacks. Each interval, the router dumps the sampled in-use heap, symbolizes it in-process, encodes it as pprof, and uploads it. Because everything happens inside the router binary, this works in any image, including distroless ones, and needs no extra Linux capabilities.

Profiles are uploaded to the agent's `/profiling/v1/input` endpoint, the same one Datadog's own profilers use. Datadog doesn't publish it as a documented API. If the agent is unreachable, slow (uploads time out after 10 seconds), or rejects an upload, the router drops that profile, without retrying or buffering, and carries on. It warns when uploads start failing and logs again when they recover. Every attempt is counted in the `apollo.router.profiling.exports` metric, with a `profiling.outcome` attribute.

Profiles are tagged from `DD_SERVICE` (default `router`), `DD_ENV`, and `DD_VERSION` (default the router's version), as Datadog's own profilers are. Turning profiling on or off takes effect on a configuration reload. Heap profiling is Linux-only, and only attributes allocations made after profiling starts. Each collection briefly uses extra memory to symbolize the profile and writes a temporary file, so the temporary directory (`TMPDIR`, default `/tmp`) must be writable.

By [@abernix](https://github.com/abernix)
