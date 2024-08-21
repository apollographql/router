### Fix cost result filtering for custom metrics ([PR #5838](https://github.com/apollographql/router/pull/5838))

Fix filtering for custom metrics that use demand control cost information in their conditions. This allows a telemetry config such as:

```yaml
telemetry:
  instrumentation:
    instruments:
      supergraph:
        cost.rejected.operations:
          type: histogram
          value:
            cost: estimated
          description: "Estimated cost per rejected operation."
          unit: delta
          condition:
            eq:
              - cost: result
              - "COST_ESTIMATED_TOO_EXPENSIVE"
```

Additionally, this fixes an issue with attribute comparisons which would silently fail to compare integers to float values. Now, users can write integer values in conditions that compare against selectors that select floats:

```yaml
telemetry:
  instrumentation:
    instruments:
      supergraph:
        cost.rejected.operations:
          type: histogram
          value:
            cost: actual
          description: "Estimated cost per rejected operation."
          unit: delta
          condition:
            gt:
              - cost: delta
              - 1
```

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5838
