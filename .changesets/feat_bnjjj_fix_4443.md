### Add selectors to extract body elements from subgraph response using JSONPath ([Issue #4443](https://github.com/apollographql/router/issues/4443))

Deprecated `subgraph_response_body` in favor of `subgraph_response_data` and `subgraph_response_errors` which is a [selector](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) and use a JSON Path to fetch data/errors from the subgraph response. 

Example:

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