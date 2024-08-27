### Fix cost result filtering for custom metrics ([PR #5838](https://github.com/apollographql/router/pull/5838))

The router can now filter for custom metrics that use demand control cost information in their conditions. This allows a telemetry config such as the following:

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

This also fixes an issue where attribute comparisons would fail silently when comparing integers to float values. Users can now write integer values in conditions that compare against selectors that select floats:

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
