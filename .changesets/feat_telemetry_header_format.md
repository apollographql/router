### Specify Trace ID Formatting ([PR #4530](https://github.com/apollographql/router/pull/4530))

This adds the ability to specify the format of the trace ID in the response headers of the supergraph service.

An example configuration making use of this feature is shown below:
```yaml
telemetry:
  apollo:
    client_name_header: name_header
    client_version_header: version_header
  exporters:
    tracing:
      experimental_response_trace_id:
        enabled: true
        header_name: trace_id
        format: decimal # Optional, defaults to hexadecimal
```

If the format is not specified, then the trace ID will continue to be in hexadecimal format.

By [@nicholascioli](https://github.com/nicholascioli) in https://github.com/apollographql/router/pull/4530
