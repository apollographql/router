### Field level metrics without FTV1 ([PR #5443](https://github.com/apollographql/router/pull/5443))

If you are an Apollo GraphOS user, field usage and lengths can be sent to apollo studio without requiring subgraphs to support FTV1.
This data is not currently displayable in GraphOS Studio.

```yaml
telemetry:
  apollo:
    experimental_local_field_metrics: true
```

There is currently a small performance impact from enabling this feature.

By [@tninesling](https://github.com/tninesling), [@geal](https://github.com/geal), [@bryn](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/5443
