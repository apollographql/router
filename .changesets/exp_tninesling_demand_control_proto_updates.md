### Add experimental field metric reporting configuration ([PR #5443](https://github.com/apollographql/router/pull/5443))

Adds an experimental configuration to report field usage metrics to GraphOS Studio without [requiring subgraphs to support federated tracing (`ftv1`)](https://www.apollographql.com/docs/federation/metrics/#how-tracing-data-is-exposed-from-a-subgraph).

The reported field usage data doesn't currently appear in GraphOS Studio.

```yaml
telemetry:
  apollo:
    experimental_local_field_metrics: true
```

There is currently a small performance impact from enabling this feature.

By [@tninesling](https://github.com/tninesling), [@geal](https://github.com/geal), [@bryn](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/5443
