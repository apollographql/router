### Add experimental support for sending traces to Studio via OTLP ([PR #4982](https://github.com/apollographql/router/pull/4982))

As the ecosystem around OpenTelemetry (OTel) has been expanding rapidly, we are evaluating a migration of Apollo's internal
tracing system to use an OTel-based protocol.

In the short-term, benefits include:

- A comprehensive way to visualize the router execution path in GraphOS Studio.
- Additional spans that were previously not included in Studio traces, such as query parsing, planning, execution, and more.
- Additional metadata such as subgraph fetch details, router idle / busy timing, and more.

Long-term, we see this as a strategic enhancement to consolidate these two disparate tracing systems.  
This will pave the way for future enhancements to more easily plug into the Studio trace visualizer.

#### Configuration

This change adds a new configuration option `experimental_otlp_tracing_sampler`. This can be used to send
a percentage of traces via OTLP instead of the native Apollo Usage Reporting protocol. Supported values:

- `always_off` (default): send all traces via Apollo Usage Reporting protocol.
- `always_on`: send all traces via OTLP.
- `0.0 - 1.0`: the ratio of traces to send via OTLP (0.5 = 50 / 50).

Note that this sampler is only applied _after_ the common tracing sampler, for example:

#### Sample 1% of traces, send all traces via OTLP:

```yaml
telemetry:
  apollo:
    # Send all traces via OTLP
    experimental_otlp_tracing_sampler: always_on

  exporters:
    tracing:
      common:
        # Sample traces at 1% of all traffic
        sampler: 0.01
```

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/4982
