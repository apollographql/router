### Adds a configuration to redact errors in traces sent to Apollo Studio

Redacts all the errors we send to Apollo Studio by default.
If you don't want to redact errors coming from traces from your subgraphs and sent to Apollo Studio you can now set `tracing.apollo.errors.subgraph.all.redact` to `false` (default is `true`).
Or if you don't want to send errors coming from traces from your subgraphs and sent to Apollo Studio you can now set `tracing.apollo.errors.subgraph.all.send` to `false` (default is `true`).

Example:

```yaml title="router.yaml"
telemetry:
  apollo:
    errors:
      subgraph:
        all:
          # Send errors to Apollo Studio
          send: true # (default: true)
          redact: false # (default: true)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3011