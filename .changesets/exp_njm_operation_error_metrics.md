### Experimental per-operation error metrics ([PR #6443](https://github.com/apollographql/router/pull/6443), [PR #6666](https://github.com/apollographql/router/pull/6666))

Adds a new experimental OpenTelemetry metric that includes error counts at a per-operation and per-client level. These metrics contain the following attributes:

- Operation name
- Operation type (query/mutation/subscription)
- Apollo operation ID
- Client name
- Client version
- Error code
- Path
- Service (subgraph name)

This metric is currently only sent to GraphOS and is not available in 3rd-party OTel destinations. The metric can be enabled using the configuration `telemetry.apollo.errors.experimental_otlp_error_metrics: enabled`.

By [@bonnici](https://github.com/bonnici) and [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/6443, https://github.com/apollographql/router/pull/6666
