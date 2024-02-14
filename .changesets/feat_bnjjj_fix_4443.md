### Replace selector to extract body elements from subgraph responses via JSONPath ([Issue #4443](https://github.com/apollographql/router/issues/4443))

The `subgraph_response_body` [selector](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) has been deprecated and replaced with selectors for a response body's constituent elements: `subgraph_response_data` and `subgraph_response_errors`.

When configuring `subgraph_response_data` and `subgraph_response_errors`, both use a JSONPath expression to fetch data or errors from a subgraph response. 

An example configuration:

```yaml
telemetry:
  instrumentation:
    spans:
      subgraph:
        attributes:
          "my_attribute":
            subgraph_response_data: "$.productName"
            subgraph_response_errors: "$.[0].message"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4579