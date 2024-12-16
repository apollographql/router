### Fix telemetry instrumentation using supergraph query selector ([PR #6324](https://github.com/apollographql/router/pull/6324))

Previously, router telemetry instrumentation that used query selectors could log errors with messages such as `this is a bug and should not happen`. 

These errors have now been fixed, and configurations with query selectors such as the following work properly:
  
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