### Telemetry instrumentation config for http_client headers ([PR #8349](https://github.com/apollographql/router/pull/8349))

Adds a new telemetry instrumentation configuration for the http_client spans. This setting allows request headers added by Rhai scripts to be attached to the http_client span. The `some_rhai_response_header` value is available on the subgraph span as before.

```yaml
telemetry:
  instrumentation:
    spans:
      mode: spec_compliant
      subgraph:
        attributes:
          http.response.header.some_rhai_response_header:
            subgraph_response_header: "some_rhai_response_header"
      http_client:
        attributes:
          http.request.header.some_rhai_request_header:
            request_header: "some_rhai_request_header"
```

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8349
