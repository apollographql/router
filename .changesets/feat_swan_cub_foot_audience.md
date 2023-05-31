### Configurable histogram buckets for metrics ([Issue #2333](https://github.com/apollographql/router/issues/2333))

It is now possible to change the default bucketing for histograms generated for metrics:

```yaml title="router.yaml"
telemetry:
  metrics:
    common:
      buckets:
        - 0.05
        - 0.10
        - 0.25
        - 0.50
        - 1.00
        - 2.50
        - 5.00
        - 10.00
        - 20.00
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3098
