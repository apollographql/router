### Place `http_client` span attributes on the `http_request` span ([PR #8798](https://github.com/apollographql/router/pull/8798))

Attributes configured under `telemetry.instrumentation.spans.http_client` are now added to the `http_request` span instead of `subgraph_request`.

Given this config:

```yaml
telemetry:
  instrumentation:
    spans:
      http_client:
        attributes:
          http.request.header.content-type:
            request_header: "content-type"
          http.response.header.content-type:
            response_header: "content-type"
```

Both attributes are now placed on the `http_request` span.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8798
