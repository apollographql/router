### Support telemetry selectors with errors ([Issue #5027](https://github.com/apollographql/router/issues/5027))

The router now supports telemetry selectors that take into account the occurrence of errors. This capability enables you to create metrics, events, or span attributes that contain error messages. 

For example, you can create a counter for the number of timed-out requests for subgraphs:
 

```yaml
telemetry:
  instrumentation:
    instruments:
      subgraph:
        requests.timeout:
          value: unit
          type: counter
          unit: request
          description: "subgraph requests containing subgraph timeout"
          attributes:
            subgraph.name: true
          condition:
            eq:
              - "request timed out"
              - error: reason
```

The router also can now compute new attributes upon receiving a new event in a supergraph response. With this capability, you can fetch data directly from the supergraph response body:

```yaml
telemetry:
  instrumentation:
    instruments:
      acme.request.on_graphql_error:
        value: event_unit
        type: counter
        unit: error
        description: my description
        condition:
          eq:
          - MY_ERROR_CODE
          - response_errors: "$.[0].extensions.code"
        attributes:
          response_errors:
            response_errors: "$.*"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5022