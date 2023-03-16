### Custom OpenTelemetry Datadog exporter mapping ([Issue #2228](https://github.com/apollographql/router/issues/2228))

This PR fixes the issue with DD exporter not providing meaningful data in the DD traces.
There is a [known issue](https://docs.rs/opentelemetry-datadog/latest/opentelemetry_datadog/#quirks) where open telemetry is not fully compatible with datadog.

To fix, this, open-telemetry-datadog added [custom mapping functions](https://docs.rs/opentelemetry-datadog/0.6.0/opentelemetry_datadog/struct.DatadogPipelineBuilder.html#method.with_resource_mapping).

when `enable_span_mapping` is set to `true`, the Apollo Router will perform the following mapping:

1. Use the Open Telemetry span name to set the Data Dog span operation name.
2. Use the Open Telemetry span attributes to set the DataDog span resource name.

Example:

Lets say we send a query `MyQuery` to the Apollo Router, then the Router using the operation's query plan will send a
query to `my-subgraph-name` and the following trace will be created:

```
    | apollo_router request                                                                 |
        | apollo_router router                                                              |
            | apollo_router supergraph                                                      |
            | apollo_router query_planning  | apollo_router execution                       |
                                                | apollo_router fetch                       |
                                                    | apollo_router subgraph                |
                                                        | apollo_router subgraph_request    |
```

As you can see, there is no clear information about the name of the Query, the name of the Subgraph, and name of Query
sent to Subgraph, when `enable_span_mapping` the following trace will be created:

```
    | request /graphql                                                                                   |
        | router                                                                                         |
            | supergraph MyQuery                                                                         |
                | query_planning MyQuery  | execution                                                    |
                                              | fetch fetch                                              |
                                                  | subgraph my-subgraph-name                            |
                                                      | subgraph_request MyQuery__my-subgraph-name__0    |
```





All this logic is gated behind a yaml configuration boolean `enable_span_mapping` which if enabled will take the values from the span attributes.

By [@samuelAndalon](https://github.com/samuelAndalon) in https://github.com/apollographql/router/pull/2790
