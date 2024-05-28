### Add graphql instruments ([PR #5215](https://github.com/apollographql/router/pull/5215))

This PR adds experimental GraphQL instruments to telemetry as a commercial feature.

It makes the following possible:
```
telemetry:
  instrumentation:
    instruments:
      graphql:
        # The number of times a field was executed (counter)
        field.execution: true

        # The length of list fields (histogram)
        list.length: true

        # Custom counter of field execution where field name = name
        "custom_counter":
          description: "count of name field"
          type: counter
          unit: "unit"
          value: field_unit
          attributes:
            graphql.type.name: true
            graphql.field.type: true
            graphql.field.name: true
          condition:
            eq:
              - field_name: string
              - "name"

        # Custom histogram of list lengths for topProducts
        "custom_histogram":
          description: "histogram of review length"
          type: histogram
          unit: "unit"
          attributes:
            graphql.type.name: true
            graphql.field.type: true
            graphql.field.name: true
          value:
            field_custom:
              list_length: value
          condition:
            eq:
              - field_name: string
              - "topProducts"
```

Note that users should not use this feature yet as it will have performance issues, may cause excessive metrics to be generated and we may change or remove this feature.
It is for experimental purposes only and is not supported in production environments.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5215
