### Fix telemetry instrumentation using supergraph query selector ([PR #6324](https://github.com/apollographql/router/pull/6324))

Query selector was raising error logs like `this is a bug and should not happen`. It's now fixed.
Now this configuration will work properly:
```yaml title=router.yaml
telemetry:
  exporters:
    metrics:
      common:
        views:
          # Define a custom view because operation limits are different than the default latency-oriented view of OpenTelemetry
          - name: oplimits.*
            aggregation:
              histogram:
                buckets:
                  - 0
                  - 5
                  - 10
                  - 25
                  - 50
                  - 100
                  - 500
                  - 1000
  instrumentation:
    instruments:
      supergraph:
        oplimits.aliases:
          value:
            query: aliases
          type: histogram
          unit: number
          description: "Aliases for an operation"
        oplimits.depth:
          value:
            query: depth
          type: histogram
          unit: number
          description: "Depth for an operation"
        oplimits.height:
          value:
            query: height
          type: histogram
          unit: number
          description: "Height for an operation"
        oplimits.root_fields:
          value:
            query: root_fields
          type: histogram
          unit: number
          description: "Root fields for an operation"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6324