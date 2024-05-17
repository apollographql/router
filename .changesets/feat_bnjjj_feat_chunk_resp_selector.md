### Add support of event responses and critical errors for selectors and instrument in telemetry ([Issue #5027](https://github.com/apollographql/router/issues/5027))

Giving the ability to compute new attributes everytime we receive a new event in supergraph response. Would be really helpful to create observability for subscriptions and defer.

I also added the support of `on_error` for selectors and especially `error` selector I added for every services. Which will let you create some metrics,events or span attributes containing error message. By adding this we will now have to ability to create a counter of request timed out for subgraphs for example:

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

And thanks to the event support for selectors you'll be able to fetch data directly from the supergraph response body:

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